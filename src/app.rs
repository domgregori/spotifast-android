//! The application: state, event handling, and the actions views ask for.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::Color32;

use crate::api::PlayRequest;
use crate::api::models::{
    PlayableItem, Queue, Track, pick_image,
};
#[cfg(test)]
use crate::api::models::{Album, Artist, Device, PlaybackState, Playlist, PlaylistItem, Show, TrackCount, User};
#[cfg(test)]
use crate::backend::PLAYLIST_PAGE_SIZE;
#[cfg(test)]
use crate::i18n::Locale;
use crate::backend::{
    ApiRequest, ApiResponse, AuthStatus, Backend, Command, Event, LocalPlayback, LyricsRequest, RecentsFor, RemoteAction,
};
use crate::wake::Waker;
use crate::i18n::gettext;
use crate::model::QueueTab;
use crate::model::*;
use crate::paths::AppDirs;
use crate::backend_ext::BackendExt;
use crate::http::Http;
use crate::images::ArtLoader;
use crate::player::{LoadSpec, LocalState, Playback, PlayerCommand, RepeatMode};
use crate::settings::{
    CachedRootlist, LanguageChoiceExt, SessionState, Settings, SettingsExt, ThemeChoice,
};
use crate::single_instance::ControlCommand;
use crate::theme::{self, Palette};
use crate::util;
use fastframe_now_playing::{
    Command as MediaCommand, NowPlaying as MediaControls, Playback as MediaPlayback,
    Repeat as MediaRepeat, State as MediaState, Track as MediaTrack,
};

/// The shortest regular gap between playback polls, which spend the Web API
/// quota.
/// How long after a command to another device, or a switch to one, polls are
/// not held to `REMOTE_POLL_MIN`, so the result is confirmed before the
/// optimistic holds run out.
/// With nobody able to see the window and nothing playing anywhere.
/// How long a suspend or the end of the session waits for pending file
/// writes. fastframe-power holds the system back for 1.5 s before a
/// suspend (3 s at the end of a session), so a longer wait would only
/// answer after the system has gone ahead without us.
/// How old the saved resume point may grow while music plays.
/// The longest the app sleeps in the tray with nothing due.
/// How long a playlist list Spotify turned away waits before it is asked
/// for again.
/// How long background work goes on before the top bar's spinner shows it,
/// so fast answers never flash it.
/// How often coming back to the window reads the playlist tree again.
/// How far into a song Previous restarts it rather than stepping back,
/// matching what librespot does during playback.

/// Match other interface animations. egui subtracts the predicted frame time
/// from delayed repaints, so 33 ms drives roughly one frame per 16 ms.

/// How long a newly started context remains visible while Spotify catches up.
/// During local takeover, Spotify may briefly alternate between old and new
/// context state.
/// How long the interface trusts its own play/pause over a polled state that
/// has not caught up yet. Spotify can take a moment to report a command it
/// has already carried out.
/// Delay before checking playback again after a command.
/// Delay before checking the queue after a local change.
/// Number of stale queue responses accepted before trusting Spotify's state.
/// Duplicate queue requests within this window count as one click.
/// How many played contexts the sidebar's Recently played order keeps.

/// The playing item as the interface sees it, whichever device plays it.
pub use spotifast_core::now_playing::NowPlaying;

/// An automatic update step put off on a metered connection or without
/// internet, taken once the connection is neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeldUpdate {
    Inspect,
    Download,
}

/// How the application is being started.
#[derive(Clone, Copy, Debug)]
pub struct AppOptions {
    pub restore_sign_in: bool,
    pub media_controls: bool,
    pub tray: bool,
}

impl Default for AppOptions {
    fn default() -> Self {
        Self { restore_sign_in: true, media_controls: true, tray: true }
    }
}

pub use spotifast_core::engine::{
    AssumedContext, Engine, Listening, PendingAlbumQueue, PendingPaste, PendingQueueAdd,
    PlaylistDetailChanges, QueueShufflePending, RemoteSnapshot, Target, TrackConfirmation, TrackIntent,
};
pub use spotifast_core::engine::{
    autoplay_seed, cap_uris, cover_error, cover_images, engine_error_text, evict_lru_map,
    fill_availability, friendly_api_error, friendly_page_error, increase_playlist_total,
    known_availability, link_recording, local_load, note_availability, page_related_needs_load,
    play_pause_label, remote_action_label, same_recording_hint,
};
pub use spotifast_core::engine::{
    ASSUMED_CONTEXT_HOLD, BACKGROUND_QUIET, CONTAINS_BATCH, DEVICES_FRESH, HEADLESS_SLEEP,
    HOME_PODCAST_SHOWS, HOME_RECENTS, OPTIMISTIC_HOLD, PLAYBACK_HOLD, PLAYLISTS_RETRY,
    QUEUE_ADD_DEBOUNCE, QUEUE_RECHECK, QUEUE_STALE_RETRIES, RECENTS_PAGE, RECENT_CONTEXTS_KEPT,
    REMOTE_COMMAND_WINDOW, REMOTE_FRESH, REMOTE_POLL_ACTIVE, REMOTE_POLL_IDLE, REMOTE_POLL_MIN,
    REMOTE_POLL_UNSEEN, REMOTE_RECHECK, RESTART_BEFORE_PREVIOUS, ROOTLIST_REFRESH, SEARCH_DEBOUNCE,
    SESSION_REFRESH, SUSPEND_FLUSH_TIMEOUT, TOAST_FRAME, TOAST_LIFETIME,
};

pub struct App {
    applied_proxy_preferences: crate::settings::ProxyPreferences,
    pending_proxy_preferences: HashMap<u64, crate::settings::ProxyPreferences>,
    /// The egui artwork loader, for the covers the interface renders.
    pub art: ArtLoader,
    media_controls: Option<MediaControls>,
    /// The desktop's light or dark preference, for "Follow system".
    #[cfg(target_os = "linux")]
    system_appearance: Option<crate::appearance::SystemAppearance>,
    /// The artwork the media controls were last given, and the URL it came
    /// from. Finding the file touches the disk and the controls are synced
    /// every frame, so the answer is kept until the artwork changes.
    media_art: Option<(String, PathBuf)>,
    tray: Option<fastframe_tray::Tray>,
    /// Whether the tray menu last offered Pause rather than Play.
    tray_playing: bool,
    /// The system's power and session state.
    power: crate::power::Power,
    /// Whether the system's word on reaching the internet goes to the
    /// backend, which then holds reconnection while there is none. macOS
    /// only: Windows's connectivity hint can say "no internet" on networks
    /// where Spotify still answers (a proxy, a blocked probe), and Linux
    /// reports nothing.
    follows_reachability: bool,
    /// The reachability the backend was last told.
    reachable: bool,
    /// Keeps local playback out of App Nap while it plays (macOS).
    playback_activity: Option<crate::power::Activity>,
    /// What the app may spend on drawing and polling, from the power state
    /// and the window's; worked out in each logic pass.
    pub budget: crate::power::Budget,
    /// How often the monitor showing the main window refreshes, in hertz,
    /// once it has said; 0 before. Not saved: the window may open on
    /// another screen next time.
    pub screen_hz: u32,
    /// Whether the desktop asks for less motion.
    pub motion: crate::motion::Motion,
    pub window_hidden: bool,
    /// The window should close but the process should stay in the tray.
    pub hide_intent: bool,
    /// The outer loop should recreate the hidden window.
    pub wants_show: bool,
    /// The window should close and reopen at once as the other kind: the
    /// big window or the Winamp mini player.
    pub switch_intent: bool,
    /// Commands from control clients (a second `spotifast <verb>` launch,
    /// a Raycast script, a link the desktop opens). Drained every frame.
    control_commands: Option<std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>>>,
    /// Now-playing snapshot for the control channel.
    control_now_playing: Option<std::sync::Arc<std::sync::Mutex<String>>>,
    /// The same, for its `devices` verb.
    control_devices: Option<std::sync::Arc<std::sync::Mutex<String>>>,
    /// Whether that device slot still matches [`Self::devices`]. The
    /// now-playing snapshot is rebuilt every frame because its position
    /// moves every frame; a device list changes when Spotify answers, which
    /// is seconds apart, so it is written when it changes instead.
    pub palette: Palette,
    /// The language the interface is drawn in: [`Settings::language`]
    /// resolved against the operating system's preferred languages.
    /// Whether this window's native backend can keep it above other windows.
    pub window_level_supported: bool,
    /// Whether this window's native backend can leave the mini player out of
    /// the taskbar: Windows and X11.
    pub taskbar_hiding_supported: bool,
    #[cfg(any(test, feature = "demo"))]
    pub demo_windows_controls: bool,
    applied_dark: Option<bool>,
    /// Reveals new colours from the middle of the window outwards.
    theme_transition: fastframe_theme::Transition,
    /// Whether colour changes are revealed. Tests that check which palette
    /// applies, on a context that never draws, turn it off.
    pub(crate) reveal_theme_changes: bool,
    pub custom_themes: theme::Catalog,
    /// Frames left to re-send the Winamp window's always-on-top level after the
    /// window opens. X11 window managers drop `_NET_WM_STATE_ABOVE` set before
    /// the window is mapped, so the creation-time level does not stick; a level
    /// pushed on the first frames after mapping does.
    winamp_level_reassert: u8,
    /// What the window's title bar says, as last set.
    window_title: String,
    pub lyrics_backdrop: crate::images::LyricsBackdrop,
    pub softened_covers: crate::images::SoftenedCovers,
    /// The main window's size, position and maximized state: from the
    /// session file, followed in each logic pass while that window is open,
    /// and given to the next one before it is made.
    pub window_geometry: fastframe_shell::window::Geometry,
    /// The mode to return the window to when the app closed in
    /// fullscreen lyrics.
    session_lyrics_fullscreen_from: Option<crate::settings::WindowMode>,
    /// Where the open dialog drew itself, so its own layout can be held
    /// to the window it has to fit inside.
    pub dialog_rect: Option<egui::Rect>,
    /// Where the MilkDrop window last was, as it reported, for restoring it.
    pub milkdrop_pos: Option<[f32; 2]>,
    /// The MilkDrop child process; `None` until it is first opened. Its
    /// `Drop` stops the child when the app does.
    #[cfg(feature = "milkdrop")]
    #[cfg(feature = "milkdrop")]
    milkdrop_host: Option<crate::milkdrop::host::Host>,
    /// Wheel step, Linux touchpad scale and glide, and the axis a gesture
    /// holds, for the window on screen.
    scrolling: fastframe_scroll::Scrolling,
    autoscroll: crate::autoscroll::Autoscroll,
    /// Last good tree and the account it belongs to, kept across restarts.
    /// A newer release than this build, once GitHub has said so.
    pub update: Option<crate::updates::Release>,
    last_update_check: Option<Instant>,
    pub update_checking: bool,
    pub show_update: bool,
    pub update_download: crate::updates::DownloadState,
    pub update_source: crate::updates::Source,
    pub update_support: Option<Result<crate::updates::Installation, String>>,
    pub update_restart_arguments: Vec<String>,
    pub update_receipt: Option<fastframe_update::Receipt>,
    held_update: Option<HeldUpdate>,
    /// Winamp window state and active skin.
    pub winamp: crate::winamp::WinampState,
    /// The spectrum behind the player bar, when that is chosen.
    pub player_bar_analyser: crate::vis::WideAnalyser,
    /// The window's pixels per point when it last drew, so artwork chosen
    /// away from the drawing code fits the display it lands on.
    pub pixels_per_point: f32,
    pub engine: Engine,
}

/// Who the desktop's media controls belong to. Links to Spotify, as
/// `spotify:` URIs or web addresses, are what they may ask Spotifast to open.
fn media_app() -> fastframe_now_playing::App {
    let mut app = fastframe_now_playing::App::new("spotifast", "Spotifast");
    app.uri_schemes = vec!["spotify".into(), "https".into(), "http".into()];
    app
}

const TRAY_SHOW: &str = "show";
const TRAY_PLAY_PAUSE: &str = "play-pause";
const TRAY_NEXT: &str = "next";
const TRAY_PREVIOUS: &str = "previous";
const TRAY_QUIT: &str = "quit";

/// What the shell around `eframe::run_native` does with the app between
/// windows.
impl fastframe_shell::Resident for App {
    /// Quit wins; switching between the main window and the mini player
    /// opens the other at once; closing to the tray runs without a window.
    fn closed(&self) -> fastframe_shell::Closed {
        use fastframe_shell::Closed;
        if self.engine.quit_requested {
            Closed::Quit
        } else if self.switch_intent {
            Closed::Reopen
        } else if self.hide_intent {
            Closed::Hide
        } else {
            Closed::Quit
        }
    }

    fn window_gone(&mut self) {
        App::window_gone(self);
    }

    /// Audio, MPRIS, the tray and polling keep running until Show or Quit.
    fn headless_frame(&mut self, ctx: &egui::Context) -> fastframe_shell::Headless {
        use fastframe_shell::Headless;
        self.background_frame(ctx);
        if self.engine.quit_requested {
            Headless::Quit
        } else if self.wants_show {
            Headless::Show
        } else {
            self.headless_wait()
        }
    }

    fn shutdown(&mut self) {
        self.save_state();
        self.engine.flush_files();
    }
}

/// The tray menu's Play or Pause entry, for what is playing.


/// The tray item: Spotifast's icon, and a menu that shows or hides the
/// window, controls playback and quits.
fn tray_config() -> fastframe_tray::Config {
    use fastframe_tray::MenuItem;
    fastframe_tray::Config {
        id: "spotifast",
        title: "Spotifast".into(),
        icon: util::app_icon_rgba,
        template_icon: Some(util::tray_template_rgba),
        themed_icon: true,
        menu: vec![
            MenuItem::action(TRAY_SHOW, "Show or hide Spotifast"),
            MenuItem::Separator,
            MenuItem::action(TRAY_PLAY_PAUSE, play_pause_label(false)),
            MenuItem::action(TRAY_NEXT, "Next"),
            MenuItem::action(TRAY_PREVIOUS, "Previous"),
            MenuItem::Separator,
            MenuItem::action(TRAY_QUIT, "Quit"),
        ],
    }
}

/// Files `uri` under the recording `key` in `track_recordings` and in its
/// reverse index, `recording_uris`, so the two always agree.












pub use spotifast_core::config::{engine_config, eq_settings};

pub use spotifast_core::util::{percent_to_volume, volume_to_percent};

/// The window level for the Winamp window's always-on-top setting. Shared by
/// window creation and the live window, so the mapping is owned in one place.
pub fn on_top_window_level(on_top: bool) -> egui::WindowLevel {
    if on_top {
        egui::WindowLevel::AlwaysOnTop
    } else {
        egui::WindowLevel::Normal
    }
}

/// The window level to push to the live window, or `None` when there is no
/// Winamp window to change. The big window (where Settings lives) keeps its
/// normal level, so toggling the setting there only takes effect once the
/// Winamp window opens.
fn winamp_on_top_level(winamp_window: bool, on_top: bool) -> Option<egui::WindowLevel> {
    winamp_window.then_some(on_top_window_level(on_top))
}





/// Availability explicitly reported for a song; session-only rows are unknown.


/// A fresh Web API answer takes precedence over anything remembered from disk.


/// Grey out the songs the Web API has said this account cannot play,
/// among rows that do not say. A page read over the streaming session
/// says nothing of the account's market; a song the Web API had greyed
/// out stays so wherever it recurs, and rows it never described stay
/// unknown.


/// The playback engine reports in English from its own thread. The
/// messages the interface recognises are shown in the user's language.






/// What the engine is told to play. A single song goes as a context of
/// its own rather than a list of one: Spotify resolves a track's URI as a
/// context, and a context with a URI is what librespot's autoplay carries
/// on from when it ends, the way one song from a search does in Spotify.


/// Returns the final track as the autoplay seed when a plain list ends.


/// Caps large track lists at 500 items starting from the selected row.








impl App {
    pub fn new(waker: &Waker, dirs: AppDirs, mut settings: Settings, options: AppOptions) -> Self {
        // The legacy password file has no endpoint of its own. Keep the old
        // settings beside it until migration binds that password in the store.
        settings.proxy_password_legacy |= dirs.proxy_secret_file().try_exists().unwrap_or(true);
        let plays = crate::history::History::load(&dirs.history_file());
        let tap = crate::vis::AudioTap::new();
        let eq = crate::eq::shared();
        if let Ok(mut shared) = eq.lock() {
            *shared = eq_settings(&settings);
        }
        let applied_proxy_preferences = settings.proxy_preferences();
        let applied_proxy = match settings.proxy_config() {
            Ok(proxy) => proxy,
            Err(error) => crate::settings::ProxyConfig::Invalid(error),
        };
        let engine_config = engine_config(
            &dirs,
            &settings,
            applied_proxy.clone(),
            std::sync::Arc::clone(&tap),
            std::sync::Arc::clone(&eq),
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("spotifast-runtime")
            .enable_all()
            .build()
            .expect("unable to start the async runtime");
        let http = if options.restore_sign_in {
            Http::unavailable("Restoring proxy settings".into())
        } else {
            Http::from_proxy(&applied_proxy).unwrap_or_else(|error| {
                log::warn!("Network configuration failed: {error}");
                Http::unavailable(error)
            })
        };
        let art = ArtLoader::new(http.clone(), runtime.handle().clone(), dirs.art_cache_dir());
        let services = std::sync::Arc::new(crate::services::DesktopServices {
            art: art.clone(),
        });
        let redirect = std::sync::Arc::new(crate::auth_redirect::LoopbackRedirectHandler);
        let backend = Backend::spawn(
            dirs.clone(),
            engine_config,
            settings.web_client_id.clone(),
            std::sync::Arc::new(waker.clone()),
            services,
            redirect,
            runtime,
            http,
            options.restore_sign_in,
        );
        let locale = settings.language.resolve();
        let applied_proxy = if options.restore_sign_in {
            crate::settings::ProxyConfig::Invalid(
                gettext(locale, "Restoring proxy settings").into_owned(),
            )
        } else {
            applied_proxy
        };
        let session = SessionState::load(&dirs.session_file());
        let wake = waker.clone();
        let media_controls = options
            .media_controls
            .then(|| MediaControls::start(media_app(), move || wake.wake()));
        #[cfg(target_os = "linux")]
        let system_appearance = {
            let wake = waker.clone();
            options
                .media_controls
                .then(|| crate::appearance::SystemAppearance::spawn(move || wake.wake()))
        };
        #[cfg(target_os = "macos")]
        let media_controls = {
            let mut media_controls = media_controls;
            if let (Some(controls), Some(track)) =
                (&mut media_controls, session.last_track.as_deref())
            {
                controls.claim(
                    track,
                    Duration::from_millis(u64::from(session.last_position_ms)),
                );
            }
            media_controls
        };
        let wake = waker.clone();
        let tray = options
            .tray
            .then(|| fastframe_tray::Tray::spawn(tray_config(), move || wake.wake()))
            .flatten();
        let power = if options.media_controls {
            let wake = waker.clone();
            crate::power::Power::spawn(move || wake.wake())
        } else {
            crate::power::Power::fixed(crate::power::Conditions::default())
        };

        let first_page = session
            .last_page
            .as_deref()
            .and_then(Page::decode)
            .filter(|page| !matches!(page, Page::Settings | Page::Queue))
            .unwrap_or(Page::Home);

        let palette = settings.cached_palette().unwrap_or_else(Palette::dark);
        let mut app = Self {
            applied_proxy_preferences,
            pending_proxy_preferences: HashMap::new(),
            art: art.clone(),
            media_controls,
            system_appearance,
            media_art: None,
            tray,
            tray_playing: false,
            power,
            follows_reachability: cfg!(target_os = "macos"),
            reachable: true,
            playback_activity: None,
            budget: crate::power::Budget::default(),
            screen_hz: 0,
            motion: crate::motion::Motion::default(),
            window_hidden: false,
            hide_intent: false,
            wants_show: false,
            switch_intent: false,
            control_commands: None,
            control_now_playing: None,
            control_devices: None,
            palette,
            window_level_supported: true,
            taskbar_hiding_supported: cfg!(windows),
            #[cfg(any(test, feature = "demo"))]
            demo_windows_controls: false,
            applied_dark: None,
            theme_transition: fastframe_theme::Transition::default(),
            reveal_theme_changes: true,
            custom_themes: theme::Catalog::default(),
            winamp_level_reassert: 0,
            window_title: String::new(),
            lyrics_backdrop: Default::default(),
            softened_covers: Default::default(),
            window_geometry: fastframe_shell::window::Geometry {
                size: session.window_size,
                position: session.window_pos,
                maximized: session.window_maximized,
            },
            session_lyrics_fullscreen_from: session.lyrics_fullscreen_from,
            dialog_rect: None,
            milkdrop_pos: session.milkdrop_pos,
            #[cfg(feature = "milkdrop")]
            #[cfg(feature = "milkdrop")]
            milkdrop_host: None,
            scrolling: fastframe_scroll::Scrolling::default(),
            autoscroll: crate::autoscroll::Autoscroll::default(),
            update: None,
            last_update_check: None,
            update_checking: false,
            show_update: false,
            update_download: crate::updates::DownloadState::Idle,
            update_source: crate::updates::Source::default(),
            update_support: None,
            update_restart_arguments: Vec::new(),
            update_receipt: None,
            held_update: None,
            winamp: crate::winamp::WinampState::new(session.winamp_pos, tap, eq),
            player_bar_analyser: crate::vis::WideAnalyser::default(),
            pixels_per_point: 1.0,

            engine: Engine {
            localize: std::sync::Arc::new(crate::i18n::GettextLocalize),
            files: std::sync::Arc::new(crate::services::FastframeFileWriter(fastframe_file::Writer::new())),
            control_devices_stale: true,
            accents: HashMap::new(),
            locale,
            dirs,
            settings,
            applied_proxy,
            proxy_request: 0,
            last_proxy_applied: 0,
            proxy_form_edited: false,
            settings_dirty: false,
            last_settings_save: Instant::now(),
            backend,
            offline: false,
            network_reachable: true,
            auth: AuthStatus::Starting,
            user: None,
            local_device_id: None,
            local_transfer_sequence: None,
            local_ready: false,
            local_playback: LocalPlayback::Unavailable,
            local: LocalState::default(),
            remote: None,
            remote_polled_at: Instant::now() - REMOTE_POLL_UNSEEN,
            remote_poll_pending: false,
            remote_end_polled_at: None,
            remote_command_at: None,
            remote_poll_seq: 0,
            session_dirty: false,
            last_session_save: Instant::now(),
            devices: Vec::new(),
            receivers: Vec::new(),
            activating_receiver: None,
            activating_play: false,
            devices_loading: false,
            devices_fetched_at: None,
            selected_device: None,
            queue: if session.last_track.is_some() && !session.last_queue_rows.is_empty() {
                // The queue as it was at close, shown until something
                // plays; then the live queue takes over.
                Loadable::Loaded(Queue {
                    currently_playing: None,
                    queue: session.last_queue_rows.clone(),
                })
            } else {
                Loadable::NotLoaded
            },
            queue_fetched_at: None,
            queue_seq: 0,
            queue_recheck_at: None,
            queue_stale_retries: 0,
            queue_cleared: None,
            queue_shuffle_pending: None,
            queue_reorder_pending: None,
            queue_removed: None,
            library: Library::default(),
            liked_songs: crate::liked::LikedSongs::default(),
            liked_recheck_at: None,
            home: HomeData::default(),
            plays,
            listening: None,
            recents: crate::model::CursorList::default(),
            recents_view: Vec::new(),
            recents_generation: 0,
            queue_tab: session
                .queue_tab
                .as_deref()
                .and_then(QueueTab::decode)
                .unwrap_or_default(),
            search: SearchState::default(),
            playlist_pages: HashMap::new(),
            playlist_cache_write_in_flight: false,
            load_generation: 0,
            album_pages: HashMap::new(),
            artist_pages: HashMap::new(),
            show_pages: HashMap::new(),
            radio_pages: HashMap::new(),
            track_cache: HashMap::new(),
            track_requests: HashSet::new(),
            album_types_requested: HashSet::new(),
            audiobook_shows: HashSet::new(),
            audiobooks_requested: HashSet::new(),
            playlist_headers: HashMap::new(),
            playlist_headers_requested: HashSet::new(),
            confirmed_ep_albums: HashSet::new(),
            table_rows: HashMap::new(),
            page_used: HashMap::new(),
            track_used: HashMap::new(),
            history: vec![first_page],
            history_index: 0,
            saved: HashMap::new(),
            saved_pending: HashSet::new(),
            track_recordings: HashMap::new(),
            recording_uris: HashMap::new(),
            playlist_availability: HashMap::new(),
            saved_recordings: HashSet::new(),
            saved_writes: HashMap::new(),
            accent_pending: HashSet::new(),
            dialog: None,
            cover_request: 0,
            cover_uploads: HashMap::new(),
            uploaded_covers: Default::default(),
            show_queue_panel: session.queue_open.unwrap_or(false),
            show_lyrics_panel: false,
            lyrics_fullscreen: None,
            lyrics_fullscreen_seen: false,
            lyrics_fullscreen_restoring: None,
            lyrics_restore_maximized: false,
            lyrics_uri: None,
            lyrics: Loadable::NotLoaded,
            lyrics_following: true,
            lyrics_line_shown: None,
            show_devices: false,
            toasts: Vec::new(),
            actions: Vec::new(),
            volume_before_mute: None,
            pending_play_keys: Vec::new(),
            pending_play_at: None,
            queued_play: None,
            local_list: None,
            pending_transfer_to: None,
            remote_recheck_at: None,
            seek_preview: None,
            volume_preview: None,
            last_eviction: Instant::now(),
            frame_now: None,
            sign_in_url: None,
            web_app: None,
            pending_remote_position: None,
            pending_remote_volume: None,
            pending_local_volume: None,
            pending_local_position: None,
            optimistic_playing: None,
            intent_track: None,
            shuffle_wanted: session.shuffle_on,
            shuffle_set_at: None,
            unavailable_at: Vec::new(),
            last_unavailable_reconnect: None,
            premium_notice_shown: false,
            assumed_context: None,
            last_now_playing_uri: None,
            last_now_playing_sequence: 0,
            queue_start_pending: None,
            playlist_busy: false,
            quit_requested: false,
            selection: None,
            table_sorts: session
                .sorts
                .iter()
                .filter_map(|(page, sort)| Some((Page::decode(page)?, *sort)))
                .collect(),
            user_names: HashMap::new(),
            user_names_revision: 0,
            user_images: HashMap::new(),
            recent_contexts: session.recent_contexts.clone(),
            resume_context: session.last_context.clone(),
            resume_track: session.last_track.clone(),
            resume_position_ms: session.last_position_ms,
            resume_queue: session.last_added_queue.clone(),
            manual_queue: Vec::new(),
            pending_queue_adds: Vec::new(),
            pending_album_queues: HashMap::new(),
            pending_queue_batches: HashMap::new(),
            album_queue_serial: 0,
            play_search_serial: 0,
            last_album_queue: None,
            rootlist: Vec::new(),
            rootlist_cache: session.rootlist.clone(),
            account_caches_generation: 0,
            account_caches_due: false,
            rootlist_read_at: None,
            window_focused: true,
            rootlist_wanted: false,
            editable_by_grant: std::collections::BTreeSet::new(),
            pending_link: None,
            prerelease_request: None,
            prerelease_serial: 0,
            copied_songs: Vec::new(),
            pending_pastes: Vec::new(),
            collapsed_folders: session.collapsed_folders.clone(),
            },
        };
        app.engine.local.volume = app.engine.settings.volume;
        // What was played here is on disk and needs nothing from the
        // network, so the tab has rows before Spotify has answered.
        app.engine.rebuild_recents();
        app
    }

    pub fn set_remote_control(&mut self, guard: &crate::single_instance::Guard) {
        self.control_commands = Some(guard.commands());
        self.control_now_playing = Some(guard.now_playing_slot());
        self.control_devices = Some(guard.devices_slot());
    }

    pub fn attach(&mut self, ctx: &egui::Context) {
        theme::install(ctx);
        ctx.add_bytes_loader(std::sync::Arc::new(self.art.clone()));
        ctx.add_image_loader(std::sync::Arc::new(self.art.scaler()));
        // The covers the last window's egui held are gone with it.
        self.art.forget_textures();
        ctx.set_theme(self.theme_preference());
        self.applied_dark = None;
        self.winamp.forget_textures();
        self.window_hidden = false;
        self.hide_intent = false;
        self.wants_show = false;
        self.switch_intent = false;
        self.winamp_level_reassert = 0;
        // A new window starts titled "Spotifast"; name the playing song
        // again rather than trust what the replaced window was told.
        self.window_title.clear();
        if let Some(tray) = &mut self.tray {
            tray.attach();
        }
        // eframe restores the full screen fullscreen lyrics left behind when
        // the app closed in them, without the lyrics. The mode they came
        // from is known, or on Windows, where only fullscreen lyrics make
        // the frameless main window full screen and nothing else could
        // leave it, it was an ordinary window.
        let lyrics_left = self.session_lyrics_fullscreen_from.take();
        if self.engine.settings.winamp_window {
            // The mini player sizes itself; the big window's geometry
            // waits here for its return. eframe may have restored the big
            // window's fullscreen/maximized state before creating this one.
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            // Re-assert the on-top level over the
            // first frames, once the window is mapped, because the level set
            // at creation does not stick on X11.
            if self.engine.settings.winamp_on_top && self.window_level_supported {
                self.winamp_level_reassert = 3;
            }
            return;
        }
        let restored_full_screen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
        let lyrics_left = lyrics_left.or_else(|| {
            (cfg!(windows) && restored_full_screen).then(crate::settings::WindowMode::default)
        });
        if let Some(mode) = lyrics_left
            && restored_full_screen
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(mode.fullscreen));
            if mode.maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
    }

    pub fn window_gone(&mut self) {
        self.winamp.forget_textures();
        self.window_hidden = true;
        self.hide_intent = false;
        self.wants_show = false;
        // Showing it again is coming back to it.
        self.engine.window_focused = false;
    }

    pub fn hides_to_tray(&self) -> bool {
        self.tray
            .as_ref()
            .is_some_and(fastframe_tray::Tray::is_shown)
            && self.engine.settings.keep_playing_in_background
    }

    pub fn art_px(&self, points: f32) -> u32 {
        crate::api::models::art_target(self.pixels_per_point, points)
    }

    pub fn now_playing_tint(&self) -> Option<Color32> {
        if !self.engine.settings.accent_from_art {
            return None;
        }
        let now = self.engine.now_playing()?;
        let url = now.art_small.or(now.art_url)?;
        self.engine.accents.get(&url).map(|&rgb| Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
    }

    fn handle_backend_events(&mut self, events: Vec<Event>) {
        for event in events {
            // Offline, as in demo mode, only colours read from cover art and
            // local update checks come through.
            if self.engine.offline
                && !matches!(&event, Event::Accent { .. })
                && (self.update_source.is_github()
                    || !matches!(
                        &event,
                        Event::UpdateChecked { .. }
                            | Event::UpdateSupport(_)
                            | Event::UpdateProgress { .. }
                            | Event::UpdateDownloaded(_)
                            | Event::UpdateInstalling(_)
                    ))
            {
                continue;
            }
            match event {
                Event::PlaylistCoverChecked {
                    id,
                    request,
                    images,
                    result,
                } => {
                    self.engine.cover_checked(&id, request, images, result);
                }
                Event::PlaylistCoverChosen {
                    id,
                    request,
                    result,
                } => {
                    self.engine.cover_chosen(&id, request, result);
                }
                Event::Auth(status) => self.engine.handle_auth(status),
                Event::Playback(status) => self.engine.handle_playback(status),
                Event::NetworkStatus(reachable) => {
                    let became_reachable = !self.engine.network_reachable && reachable;
                    self.engine.network_reachable = reachable;
                    if became_reachable {
                        self.engine.on_network_restored();
                    }
                }
                Event::Receivers(receivers) => self.engine.receivers = receivers,
                Event::ReceiverActivated { name, result } => {
                    self.engine.activating_receiver = None;
                    match result {
                        Ok(()) => {
                            self.engine.toast(
                                // Translators: {name} is the name of a speaker or other playback device.
                                self.engine.localize.t(self.engine.locale, "{name} is ready").replace("{name}", &name),
                            );
                            // It takes a moment to appear in the device list.
                            self.engine.pending_transfer_to = Some((name, Instant::now()));
                            self.engine.devices_fetched_at = None;
                            self.engine.refresh_devices();
                        }
                        Err(error) => self.engine.toast_error(format!("{name}: {error}")),
                    }
                }
                Event::Local(state) => self.handle_local(*state),
                Event::Api(response) => self.handle_api(*response),
                Event::Accent { url, color } => {
                    self.engine.accent_pending.remove(&url);
                    self.engine.accents.insert(url, color);
                }
                Event::ProxyRestored { config, password } => {
                    self.engine.handle_proxy_restored(config, password)
                }
                Event::ProxyPasswordStored => self.handle_proxy_password_stored(),
                Event::ProxyStorageFailed(error) => {
                    let message = if self.engine.settings.proxy_password_legacy {
                        self.engine.localize.t(
                            self.engine.locale,
                            // Translators: {error} is a sentence saying why the proxy password could not be stored.
                            "{error} The original settings file is kept intact; changed preferences are not saved yet.",
                        )
                        .replace("{error}", error.proxy_message())
                    } else {
                        error.proxy_message().to_string()
                    };
                    self.engine.toast_error(message);
                }
                Event::ProxyApplied {
                    request,
                    config,
                    result,
                } => self.handle_proxy_applied(request, config, result),
                Event::Error(message) => self.engine.toast_error(message),
                Event::Rootlist { result } => match result {
                    Ok(rootlist) => {
                        let account_id = self.engine.user_id().map(str::to_owned).or_else(|| {
                            if let AuthStatus::Connected { username } = &self.engine.auth {
                                Some(username.clone())
                            } else {
                                None
                            }
                        });
                        self.engine.rootlist = rootlist.entries;
                        self.engine.editable_by_grant = rootlist.editable;
                        if let Some(account_id) = account_id {
                            self.engine.rootlist_cache = Some(CachedRootlist {
                                account_id,
                                entries: self.engine.rootlist.clone(),
                            });
                            self.engine.session_dirty = true;
                        }
                    }
                    Err(error) => log::warn!("rootlist unavailable: {error}"),
                },
                Event::Lyrics { uri, result } => {
                    if self.engine.lyrics_uri.as_deref() == Some(uri.as_str()) {
                        self.engine.lyrics = match result {
                            Ok(found) => Loadable::Loaded(found),
                            Err(error) => Loadable::Failed(error),
                        };
                    }
                }
                Event::PlaylistCache {
                    account_id,
                    id,
                    generation,
                    cache,
                } => {
                    self.engine.receive_playlist_cache(&account_id, &id, generation, cache);
                }
                Event::PlaylistCacheStored {
                    account_id,
                    id,
                    generation,
                    snapshot,
                    success,
                } => {
                    self.engine.receive_playlist_cache_stored(
                        &account_id,
                        &id,
                        generation,
                        &snapshot,
                        success,
                    );
                }
                Event::LikedSongsCache {
                    account_id,
                    generation,
                    cache,
                } => {
                    self.engine.receive_liked_cache(&account_id, generation, cache);
                }
                Event::AccountCaches {
                    account_id,
                    generation,
                    playlists,
                    home,
                } => {
                    self.engine.receive_account_caches(&account_id, generation, playlists, home);
                }
                Event::UserName { id, name, image } => {
                    self.engine.set_user_name(id.clone(), name);
                    self.engine.set_user_image(id, image);
                }
                Event::AudiobookShows(uris) => {
                    self.engine.audiobook_shows.extend(uris);
                }
                Event::PlaylistHeader(header) => {
                    self.engine.playlist_headers.insert(header.id.clone(), *header);
                    self.engine.fill_blank_playlists();
                }
                Event::Radio {
                    seed,
                    generation,
                    result,
                } => self.engine.receive_radio(&seed, generation, result),
                Event::Prerelease {
                    uri,
                    generation,
                    result,
                } => self.engine.receive_prerelease(&uri, generation, result.map(|data| *data)),
                Event::PrereleaseSaved {
                    uri,
                    saved,
                    generation,
                    result,
                } => self.engine.receive_prerelease_saved(&uri, saved, generation, result),
                Event::AlbumType { uri, result } => match result {
                    Ok(true) => {
                        self.engine.confirmed_ep_albums.insert(uri);
                    }
                    Ok(false) => {}
                    Err(error) => log::debug!("album type unavailable for {uri}: {error}"),
                },
                Event::WebApp { client_id } => self.engine.web_app = client_id,
                Event::UpdateSupport(result) => {
                    if result.is_ok()
                        && self.engine.settings.download_updates_automatically
                        && matches!(self.update_download, crate::updates::DownloadState::Idle)
                    {
                        self.take_update_step(HeldUpdate::Download);
                    }
                    self.update_support = Some(result);
                }
                Event::UpdateProgress { received, total } => {
                    self.update_download =
                        crate::updates::DownloadState::Downloading { received, total };
                }
                Event::UpdateDownloaded(result) => {
                    self.update_download = match result {
                        Ok(prepared) => crate::updates::DownloadState::Ready(prepared),
                        Err(error) => crate::updates::DownloadState::Failed(error),
                    };
                }
                Event::UpdateInstalling(result) => match result {
                    Ok(()) => self.engine.actions.push(Action::Quit),
                    Err(error) => {
                        self.update_download = crate::updates::DownloadState::Failed(error)
                    }
                },
                Event::UpdateChecked { manual, result } => {
                    self.update_checking = false;
                    match result {
                        Ok(Some(notice)) => {
                            if manual || self.update.as_ref() != Some(&notice) {
                                self.engine.toast(
                                    // Translators: {version} is a version number such as 1.4.0.
                                    self.engine.localize.t(self.engine.locale, "Spotifast {version} is available")
                                        .replace("{version}", &notice.version.to_string()),
                                );
                            }
                            self.update = Some(notice);
                            if self.engine.settings.download_updates_automatically
                                && matches!(
                                    self.update_download,
                                    crate::updates::DownloadState::Idle
                                )
                            {
                                self.take_update_step(HeldUpdate::Inspect);
                            }
                        }
                        Ok(None) => {
                            self.update = None;
                            if manual {
                                self.engine.toast(self.engine.localize.t(self.engine.locale, "Spotifast is up to date"));
                            } else {
                                log::debug!("this is the newest release");
                            }
                        }
                        Err(error) if manual => {
                            self.engine.toast_error(
                                // Translators: {error} is an error message.
                                self.engine.localize.t(self.engine.locale, "Couldn't check for updates: {error}")
                                    .replace("{error}", &error.to_string()),
                            );
                        }
                        Err(error) => {
                            log::debug!("could not check for a newer release: {error}");
                        }
                    }
                }
            }
        }
    }

    fn push_winamp_level(&self, ctx: &egui::Context) {
        if self.window_level_supported
            && let Some(level) =
                winamp_on_top_level(self.engine.settings.winamp_window, self.engine.settings.winamp_on_top)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        self.poll_custom_themes(ctx);
        if let Some(wait) = self.window_focus(ctx.input(|input| {
            // A minimised window is away even where it keeps focus.
            let viewport = input.viewport();
            if viewport.minimized == Some(true) {
                Some(false)
            } else {
                viewport.focused
            }
        })) {
            ctx.request_repaint_after(wait);
        }
        let now = Instant::now();
        if self.winamp_level_reassert > 0 {
            self.winamp_level_reassert -= 1;
            self.push_winamp_level(ctx);
            // The window can be idle right after opening, so drive the next
            // frame to make sure the re-assert actually runs.
            if self.winamp_level_reassert > 0 {
                ctx.request_repaint();
            }
        }
        // The zoom is the main window's: a pass without a window, or in the
        // mini player at its own scale, neither applies nor saves it.
        let main_window = !self.window_hidden && !self.engine.settings.winamp_window;
        // Each new main window takes the saved zoom; Ctrl+plus/minus is
        // remembered for the next start.
        if main_window
            && let Some(zoom) = fastframe_shell::window::keep_zoom(ctx, self.engine.settings.zoom)
        {
            self.engine.settings.zoom = zoom;
            self.engine.settings_dirty = true;
        }
        self.engine.toasts
            .retain(|toast| toast.created.elapsed() < TOAST_LIFETIME);
        self.engine.maybe_suggest_personal_app();

        // Not on a metered connection or without internet: the check waits
        // for a connection that will do, and checking by hand still works.
        if self.engine.settings.check_for_updates
            && !self.engine.offline
            && !self.downloads_wait()
            && self
                .last_update_check
                .is_none_or(|at| at.elapsed() >= crate::updates::CHECK_INTERVAL)
        {
            self.check_for_updates(false);
        }
        // The step stays held while automatic downloads are off, so turning
        // them back on picks it up.
        if !self.downloads_wait()
            && self.engine.settings.download_updates_automatically
            && matches!(self.update_download, crate::updates::DownloadState::Idle)
            && let Some(step) = self.held_update.take()
        {
            self.take_update_step(step);
        }

        if self.engine.is_connected() && !self.engine.offline {
            self.engine.request_resume_track();
            self.engine.ensure_resume_context_loaded();
        }

        // The spinner appears once background work outlasts the quiet
        // spell, even when nothing else repaints by then.
        if let Some(remaining) = self
            .background_work_since()
            .into_iter()
            .flatten()
            .map(|since| since.elapsed())
            .filter(|elapsed| *elapsed < BACKGROUND_QUIET)
            .map(|elapsed| BACKGROUND_QUIET - elapsed)
            .min()
        {
            ctx.request_repaint_after(remaining);
        }

        if let Some(at) = self.engine.library.playlists_retry_at {
            if !self.engine.is_connected() || self.engine.offline {
                self.engine.library.playlists_retry_at = None;
            } else if !self.engine.network_reachable {
                // The network coming back asks again.
            } else if Instant::now() >= at {
                self.engine.load_playlists();
            } else {
                ctx.request_repaint_after(at - Instant::now());
            }
        }

        if self.engine.is_connected() && !self.engine.offline {
            self.poll_remote_if_due();
            if let Some(due) = self.engine.remote_recheck_at
                && Instant::now() >= due
            {
                self.engine.remote_recheck_at = None;
                self.engine.poll_remote(true);
            }
            if self.engine.show_devices
                && !self.engine.devices_loading
                && self.engine.devices_fetched_at
                    .is_none_or(|at| at.elapsed() > DEVICES_FRESH)
            {
                self.engine.refresh_devices();
            }
            let playlist_open = self.engine.settings.winamp_window && self.engine.settings.playlist_open;
            if (self.engine.show_queue_panel || matches!(self.engine.page(), Page::Queue) || playlist_open)
                && !self.engine.queue.is_loading()
                && self.engine.queue_fetched_at
                    .is_none_or(|at| at.elapsed() > Duration::from_secs(20))
            {
                self.engine.refresh_queue(false);
            }
            if let Some(due) = self.engine.liked_recheck_at {
                if Instant::now() >= due && !self.engine.liked_songs.refreshing() {
                    self.engine.liked_recheck_at = None;
                    self.engine.refresh_liked_songs();
                } else {
                    ctx.request_repaint_after(
                        due.saturating_duration_since(Instant::now())
                            .max(Duration::from_millis(100)),
                    );
                }
            }
            if let Some(due) = self.engine.queue_recheck_at {
                if Instant::now() >= due {
                    self.engine.queue_recheck_at = None;
                    self.engine.refresh_queue(true);
                } else {
                    ctx.request_repaint_after(
                        (due - Instant::now()).max(Duration::from_millis(50)),
                    );
                }
            }
        }

        if let Some(typed) = self.engine.search.typed_at {
            if typed.elapsed() >= SEARCH_DEBOUNCE {
                self.engine.search.typed_at = None;
                let query = self.engine.search.query.trim().to_string();
                self.engine.run_typed_search(query);
            } else {
                ctx.request_repaint_after(SEARCH_DEBOUNCE - typed.elapsed());
            }
        }

        if self.engine.last_eviction.elapsed() > Duration::from_secs(20) {
            self.engine.last_eviction = now;
            self.art.evict(ctx);
            self.engine.evict_stale_pages();
        }
        self.sync_skin(ctx);
        if self.engine.settings_dirty && self.engine.last_settings_save.elapsed() > Duration::from_secs(2) {
            self.save_settings();
        }
        // The resume point moves while music plays, and nothing else marks the
        // session for saving then. Refresh it now and again so a session that
        // ends without a goodbye (a shutdown, a crash) resumes close by.
        if self.engine.now_playing().is_some_and(|now| now.playing)
            && self.engine.last_session_save.elapsed() > SESSION_REFRESH
        {
            self.engine.session_dirty = true;
        }
        if self.engine.session_dirty && self.engine.last_session_save.elapsed() > Duration::from_secs(2) {
            self.save_session();
        }
    }

    fn sync_skin(&mut self, ctx: &egui::Context) {
        if self.engine.settings.winamp_window
            && !self.winamp.is_loading()
            && self.winamp.worn != self.engine.settings.skin
        {
            match self.engine.settings.skin.clone() {
                None => self.winamp.wear(None, crate::skin::Skin::builtin()),
                Some(name) => self.winamp.load(name, &self.engine.dirs.skins_dir(), ctx),
            }
        }
        if let Some(loaded) = self.winamp.poll() {
            self.skin_loaded(loaded);
        }
        let fetched = self.winamp.presets.poll();
        if let Some(fetched) = fetched {
            match fetched {
                Ok(count) => {
                    self.engine.toast(
                        self.engine.localize.tn(
                            self.engine.locale,
                            // Translators: {count} is the number of visualizer presets added.
                            "Added {count} MilkDrop preset",
                            "Added {count} MilkDrop presets",
                            count as u32,
                        )
                        .replace("{count}", &count.to_string()),
                    );
                    // Restart the child so it loads the new preset list.
                    #[cfg(feature = "milkdrop")]
                    if let Some(host) = self.milkdrop_host.as_mut()
                        && host.is_running()
                    {
                        host.close();
                    }
                }
                Err(error) => self.engine.toast_error(
                    // Translators: {error} is an error message.
                    self.engine.localize.t(self.engine.locale, "Couldn't fetch presets: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            }
        }
    }

    #[cfg(feature = "milkdrop")]
    fn sync_milkdrop(&mut self, ctx: &egui::Context) {
        let presets = self.engine.dirs.milkdrop_dir();
        let open = self.engine.settings.milkdrop_open;
        let size = self.engine.settings.milkdrop_size;
        let pos = self.milkdrop_pos;
        let fullscreen = self.engine.settings.milkdrop_fullscreen;
        let fps = crate::power::milkdrop_fps(self.power.conditions(), self.engine.settings.milkdrop_fps);
        let seconds = self.engine.settings.milkdrop_seconds;
        let scale = self.engine.settings.milkdrop_scale.max(1);
        // Track metadata shown when the song changes.
        let song = self.engine.now_playing().filter(|now| !now.resuming).map(|now| {
            // Title, artist, and album.
            vec![
                now.title.clone(),
                now.subtitle.clone(),
                now.album_name.clone(),
            ]
        });
        if self.milkdrop_host.is_none() {
            let tap = std::sync::Arc::clone(&self.winamp.tap);
            self.milkdrop_host = Some(crate::milkdrop::host::Host::new(tap));
        }
        let poll = {
            let host = self.milkdrop_host.as_mut().expect("the host was just made");
            if open {
                if !host.is_running() {
                    host.open(&presets, size, pos, fullscreen, fps, seconds, scale);
                }
                host.update(fps, seconds, scale);
                host.song(song);
            } else if host.is_running() {
                host.close();
            }
            host.poll()
        };
        if poll.closed {
            self.engine.settings.milkdrop_open = false;
            self.engine.mark_settings_dirty();
        }
        if let Some(size) = poll.size
            && self.engine.settings.milkdrop_size != size
        {
            self.engine.settings.milkdrop_size = size;
            self.engine.mark_settings_dirty();
        }
        if let Some(pos) = poll.pos {
            self.milkdrop_pos = Some(pos);
        }
        for command in poll.commands {
            self.milkdrop_command(&command);
        }
        if let Some(hz) = poll.screen_hz {
            self.learn_screen_hz(hz);
        }
        // Poll the child while the main window is otherwise idle.
        if self.engine.settings.milkdrop_open {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }

    pub fn visualiser_fps(&self) -> Option<u32> {
        crate::power::visualiser_fps(
            self.budget,
            self.power.conditions(),
            self.engine.settings.visualizer_fps,
        )
    }

    pub fn visualiser_screen_hz(&self) -> u32 {
        match self.screen_hz {
            0 => self.engine.settings.milkdrop_screen_hz,
            hz => hz,
        }
    }

    #[allow(dead_code)]
    fn learn_screen_hz(&mut self, hz: u32) {
        if !crate::milkdrop::is_screen_rate(hz) || self.engine.settings.milkdrop_screen_hz == hz {
            return;
        }
        let first = self.engine.settings.milkdrop_screen_hz == 0
            && self.engine.settings.milkdrop_fps == crate::milkdrop::DEFAULT_FPS;
        self.engine.settings.milkdrop_screen_hz = hz;
        if first {
            self.engine.settings.milkdrop_fps = hz;
        }
        self.engine.mark_settings_dirty();
    }

    fn skin_loaded(&mut self, loaded: crate::winamp::Loaded) {
        match loaded.result {
            Ok(skin) => {
                self.winamp
                    .wear(Some(loaded.name.clone()), std::sync::Arc::new(skin));
                if loaded.installed {
                    self.engine.toast(
                        // Translators: {skin} is the name of a Winamp skin.
                        self.engine.localize.t(self.engine.locale, "Added {skin} skin")
                            .replace("{skin}", crate::winamp::label(&loaded.name)),
                    );
                    self.winamp.list_choices(&self.engine.dirs.skins_dir());
                    self.engine.settings.skin = Some(loaded.name);
                    self.engine.settings_dirty = true;
                }
            }
            Err(error) => {
                self.engine.toast_error(format!("{}: {error}", crate::winamp::label(&loaded.name)));
                if !loaded.installed {
                    self.engine.settings.skin = self.winamp.worn.clone();
                    self.engine.settings_dirty = true;
                }
            }
        }
    }

    fn push_eq(&mut self) {
        if let Ok(mut shared) = self.winamp.eq.lock() {
            *shared = eq_settings(&self.engine.settings);
        }
        self.engine.settings_dirty = true;
    }

    pub fn load_custom_themes(&mut self, waker: &Waker) {
        let waker = waker.clone();
        self.custom_themes.start(
            self.engine.dirs.config.join("themes"),
            self.engine.settings.custom_theme.clone(),
            &fastframe_theme::Waker::new(move || waker.wake()),
        );
    }

    pub fn enable_desktop_themes(&mut self) {
        theme::enable_desktop_themes(&mut self.custom_themes);
    }

    fn poll_custom_themes(&mut self, ctx: &egui::Context) {
        // The themes folder or Omarchy's current theme changed on disk.
        if self.custom_themes.needs_reload() {
            self.engine.actions.push(Action::ReloadThemes);
        }
        if self.custom_themes.poll() {
            self.adopt_custom_themes(ctx);
        }
    }

    fn adopt_custom_themes(&mut self, ctx: &egui::Context) {
        let mut changed = false;
        if let Some(filename) = &self.engine.settings.custom_theme
            && let Some(theme) = self.custom_themes.find(filename)
            && self.engine.settings.custom_theme_cache.as_ref() != serde_json::to_value(theme).ok().as_ref()
        {
            // Use the current selection, never the selection captured by the scan.
            self.engine.settings.custom_theme_cache = serde_json::to_value(theme).ok();
            changed = true;
        }
        if self.custom_themes.follows_omarchy() {
            if let Some(theme) = self.custom_themes.system_theme()
                && self.engine.settings.system_theme_cache.as_ref() != serde_json::to_value(theme).ok().as_ref()
            {
                self.engine.settings.system_theme_cache = serde_json::to_value(theme).ok();
                changed = true;
            }
        } else if self.engine.settings.system_theme_cache.take().is_some() {
            changed = true;
        }
        if changed {
            self.engine.mark_settings_dirty();
            ctx.set_theme(self.theme_preference());
        }
    }

    fn theme_preference(&self) -> egui::ThemePreference {
        if let Some(palette) = self.custom_palette() {
            return if palette.dark {
                egui::ThemePreference::Dark
            } else {
                egui::ThemePreference::Light
            };
        }
        match self.engine.settings.theme {
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::System => egui::ThemePreference::System,
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        // winit reports no system theme on Linux, so "Follow system" falls
        // back to what the desktop portal says.
        #[cfg(target_os = "linux")]
        if let Some(dark) = self
            .system_appearance
            .as_ref()
            .and_then(crate::appearance::SystemAppearance::dark)
        {
            let theme = if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            };
            if ctx.options(|options| options.fallback_theme) != theme {
                ctx.options_mut(|options| options.fallback_theme = theme);
            }
        }
        let dark = ctx.theme() == egui::Theme::Dark;
        let palette = self.custom_palette().unwrap_or_else(|| {
            if dark {
                Palette::dark()
            } else {
                Palette::light()
            }
        });
        if self.applied_dark != Some(dark) || self.palette != palette {
            // The first colours need no reveal, and the mini player's window
            // is drawn by its skin.
            if self.reveal_theme_changes
                && !self.motion.reduced()
                && self.applied_dark.is_some()
                && !self.engine.settings.winamp_window
            {
                self.theme_transition.begin(ctx);
                if self.theme_transition.holding(ctx) {
                    return;
                }
            }
            self.palette = palette;
            theme::apply(ctx, &self.palette);
            crate::motion::apply(ctx, self.motion.reduced());
            self.applied_dark = Some(dark);
            self.engine.accents.clear();
            self.engine.accent_pending.clear();
        }
    }

    fn handle_tray(&mut self) {
        use fastframe_tray::Event;
        let Some(events) = self.tray.as_ref().map(fastframe_tray::Tray::events) else {
            return;
        };
        for event in events {
            let action = match event {
                Event::Show => Action::ShowWindow,
                Event::Toggle | Event::Menu(TRAY_SHOW) => {
                    if self.window_hidden {
                        Action::ShowWindow
                    } else {
                        Action::HideWindow
                    }
                }
                Event::Menu(TRAY_PLAY_PAUSE) => Action::TogglePlay,
                Event::Menu(TRAY_NEXT) => Action::Next,
                Event::Menu(TRAY_PREVIOUS) => Action::Previous,
                Event::Menu(TRAY_QUIT) => Action::Quit,
                Event::Menu(_) => continue,
            };
            self.engine.actions.push(action);
        }
    }

    fn handle_control_commands(&mut self) {
        let Some(queue) = &self.control_commands else {
            return;
        };
        let commands: Vec<ControlCommand> =
            std::mem::take(&mut *queue.lock().unwrap_or_else(|p| p.into_inner()));
        for command in commands {
            let playing = self.engine.now_playing().is_some_and(|now| now.playing);
            let action = match command {
                ControlCommand::Show => Some(Action::ShowWindow),
                ControlCommand::ReloadThemes => Some(Action::ReloadThemes),
                ControlCommand::PlayPause => Some(Action::TogglePlay),
                ControlCommand::Play => (!playing).then_some(Action::TogglePlay),
                ControlCommand::Pause => playing.then_some(Action::TogglePlay),
                ControlCommand::Next => Some(Action::Next),
                ControlCommand::Previous => Some(Action::Previous),
                ControlCommand::SeekBy(offset) => Some(Action::SeekBy(offset)),
                ControlCommand::VolumeBy(delta) => Some(Action::VolumeBy(delta)),
                ControlCommand::SetVolume(volume) => Some(Action::SetVolume(volume.min(100))),
                ControlCommand::ToggleMute => Some(Action::ToggleMute),
                ControlCommand::ToggleShuffle => Some(Action::ToggleShuffle),
                ControlCommand::CycleRepeat => Some(Action::CycleRepeat),
                ControlCommand::SetShuffle(shuffle) => Some(Action::SetShuffle(shuffle)),
                ControlCommand::SetRepeat(mode) => Some(Action::SetRepeat(mode)),
                ControlCommand::SeekTo(position) => Some(Action::Seek(position)),
                // Nothing playing is nothing to save, so the verb is a
                // no-op rather than an error the client has to handle.
                ControlCommand::ToggleSaved => {
                    self.engine.now_playing().map(|now| Action::ToggleSaved(now.uri))
                }
                ControlCommand::PlayUri(uri) => Some(Action::PlayContext {
                    uri,
                    offset_uri: None,
                    offset_index: None,
                }),
                ControlCommand::OpenLink(uri) => Some(Action::OpenLink(uri)),
                ControlCommand::Transfer(device_id) => Some(Action::Transfer(device_id)),
                ControlCommand::PlaySearch { kind, query } => {
                    Some(Action::PlaySearch { kind, query })
                }
                ControlCommand::RefreshDevices => Some(Action::RefreshDevices),
            };
            if let Some(action) = action {
                self.engine.actions.push(action);
            }
        }
    }

    fn handle_media_commands(&mut self) {
        let Some(commands) = self.media_controls.as_ref().map(MediaControls::commands) else {
            return;
        };
        for command in commands {
            let playing = self.engine.now_playing().is_some_and(|now| now.playing);
            let action = match command {
                MediaCommand::Play => (!playing).then_some(Action::TogglePlay),
                MediaCommand::Pause | MediaCommand::Stop => playing.then_some(Action::TogglePlay),
                MediaCommand::PlayPause => Some(Action::TogglePlay),
                MediaCommand::Next => Some(Action::Next),
                MediaCommand::Previous => Some(Action::Previous),
                MediaCommand::SeekBy(offset) => Some(Action::SeekBy(offset)),
                MediaCommand::SetPosition { track_id, position } => self.engine.now_playing()
                    .filter(|now| now.uri == track_id)
                    .map(|_| Action::Seek(u32::try_from(position.as_millis()).unwrap_or(u32::MAX))),
                MediaCommand::SetVolume(volume) => Some(Action::SetVolume(
                    (volume.clamp(0.0, 1.0) * 100.0).round() as u8,
                )),
                MediaCommand::SetShuffle(shuffle) => Some(Action::SetShuffle(shuffle)),
                MediaCommand::SetRepeat(mode) => Some(Action::SetRepeat(match mode {
                    MediaRepeat::Off => RepeatMode::Off,
                    MediaRepeat::Track => RepeatMode::Track,
                    MediaRepeat::Playlist => RepeatMode::Context,
                })),
                MediaCommand::OpenUri(uri) => {
                    if crate::link::search_query(&uri).is_some() {
                        crate::link::parse(&uri).map(Action::OpenLink)
                    } else {
                        Some(Action::PlayContext {
                            uri,
                            offset_uri: None,
                            offset_index: None,
                        })
                    }
                }
                MediaCommand::Raise => Some(Action::ShowWindow),
                MediaCommand::Quit => Some(Action::Quit),
            };
            if let Some(action) = action {
                self.engine.actions.push(action);
            }
        }
    }

    fn media_art_file(&mut self, ctx: &egui::Context, url: &str) -> Option<PathBuf> {
        if cfg!(target_os = "linux") {
            return None;
        }
        if let Some((known, file)) = &self.media_art
            && known == url
        {
            return Some(file.clone());
        }
        match self.art.cached_file(url) {
            Some(file) => {
                self.media_art = Some((url.to_owned(), file.clone()));
                Some(file)
            }
            // On a metered connection the controls go without the large
            // cover rather than download one for every song.
            None => {
                if !self.metered() {
                    self.art.prefetch(ctx, url);
                }
                None
            }
        }
    }

    pub fn windows_controls_visible(&self) -> bool {
        #[cfg(any(test, feature = "demo"))]
        {
            cfg!(windows) || self.demo_windows_controls
        }
        #[cfg(not(any(test, feature = "demo")))]
        {
            cfg!(windows)
        }
    }

    pub fn taskbar_setting_visible(&self) -> bool {
        self.taskbar_hiding_supported || self.windows_controls_visible()
    }

    fn sync_media_controls(&mut self, ctx: &egui::Context) {
        let art_file = self.engine.now_playing()
            .and_then(|now| now.art_url)
            .and_then(|url| self.media_art_file(ctx, &url));
        let state = match self.engine.now_playing() {
            Some(now) => MediaState {
                // Still loading reads as paused: Play resumes it.
                playback: if now.playing {
                    MediaPlayback::Playing
                } else {
                    MediaPlayback::Paused
                },
                track: Some(MediaTrack {
                    id: now.uri.clone(),
                    title: now.title.clone(),
                    artists: now
                        .artists
                        .iter()
                        .map(|artist| artist.name.clone())
                        .collect(),
                    album: now.album_name.clone(),
                    duration: Some(Duration::from_millis(u64::from(now.duration_ms))),
                    art_file,
                    art_url: now.art_url.clone(),
                    url: Some(now.uri.clone()),
                    ..MediaTrack::default()
                }),
                position: Duration::from_millis(u64::from(now.position_ms)),
                volume: Some(f64::from(now.volume_percent) / 100.0),
                shuffle: Some(now.shuffle),
                repeat: Some(match now.repeat {
                    RepeatMode::Off => MediaRepeat::Off,
                    RepeatMode::Track => MediaRepeat::Track,
                    RepeatMode::Context => MediaRepeat::Playlist,
                }),
                ..MediaState::default()
            },
            None => MediaState {
                volume: Some(1.0),
                shuffle: Some(false),
                repeat: Some(MediaRepeat::Off),
                ..MediaState::default()
            },
        };
        if let Some(controls) = &mut self.media_controls {
            controls.update(state);
        }
        let playing = self.engine.now_playing().is_some_and(|now| now.playing);
        if let Some(tray) = &mut self.tray
            && self.tray_playing != playing
        {
            self.tray_playing = playing;
            tray.set_label(TRAY_PLAY_PAUSE, play_pause_label(playing));
        }
        #[cfg(target_os = "macos")]
        crate::mac_menu::set_playing(playing);
        if let Some(slot) = &self.control_now_playing {
            let snapshot = self.control_snapshot();
            *slot.lock().unwrap_or_else(|p| p.into_inner()) = snapshot;
        }
        if self.engine.control_devices_stale
            && let Some(slot) = self.control_devices.clone()
        {
            let snapshot = self.control_devices_snapshot();
            *slot.lock().unwrap_or_else(|p| p.into_inner()) = snapshot;
            self.engine.control_devices_stale = false;
        }
    }

    fn control_snapshot(&self) -> String {
        let Some(now) = self.engine.now_playing() else {
            return crate::single_instance::NOTHING_PLAYING.to_owned();
        };
        let state = if now.playing { "playing" } else { "paused" };
        // Not every track has been looked up yet; say so rather than
        // claiming an unsaved track the client would draw as a hollow heart
        // and then watch fill in a moment later.
        let saved = match self.engine.is_saved(&now.uri) {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        // Local playback is this computer, which Spotify has not named in
        // the snapshot because it is not a remote device.
        let device = match (&now.device_name, now.local) {
            (Some(name), _) => name.as_str(),
            (None, true) => self.engine.settings.device_name.as_str(),
            (None, false) => "",
        };
        // Tabs separate the fields, so a tab inside one would shift the rest.
        // This runs every frame, and titles almost never contain one, so the
        // usual answer borrows rather than allocating a copy per field.
        fn clean(text: &str) -> std::borrow::Cow<'_, str> {
            match text.contains('\t') {
                true => std::borrow::Cow::Owned(text.replace('\t', " ")),
                false => std::borrow::Cow::Borrowed(text),
            }
        }
        format!(
            "{state}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{saved}\t{}",
            clean(&now.title),
            clean(&now.subtitle),
            clean(&now.album_name),
            now.position_ms,
            now.duration_ms,
            now.volume_percent,
            if now.shuffle { "on" } else { "off" },
            now.repeat.api_name(),
            clean(now.art_url.as_deref().unwrap_or_default()),
            clean(device),
        )
    }

    fn control_devices_snapshot(&self) -> String {
        let devices: Vec<_> = self.engine.devices
            .iter()
            .filter_map(|device| {
                Some(serde_json::json!({
                    "id": device.id.as_deref()?,
                    "name": device.name,
                    "kind": device.kind,
                    "active": device.is_active,
                }))
            })
            .collect();
        serde_json::to_string(&devices)
            .unwrap_or_else(|_| crate::single_instance::NO_DEVICES.to_owned())
    }

    fn window_focus(&mut self, focused: Option<bool>) -> Option<Duration> {
        // Frames without a window have no focus to report.
        if self.window_hidden {
            return None;
        }
        if let Some(focused) = focused {
            if focused && !self.engine.window_focused {
                self.engine.rootlist_wanted = true;
            }
            self.engine.window_focused = focused;
        }
        // Until every playlist has arrived, the first read is still to come.
        if !self.engine.rootlist_wanted
            || !self.engine.is_connected()
            || !matches!(self.engine.library.playlists, Loadable::Loaded(_))
            || self.engine.library.playlists_next.is_some()
            || self.engine.library.playlists_asked.is_some()
            || self.engine.library.playlists_loading_since.is_some()
        {
            return None;
        }
        let wait = self.engine.rootlist_read_at.map_or(Duration::ZERO, |at| {
            ROOTLIST_REFRESH.saturating_sub(at.elapsed())
        });
        if wait.is_zero() {
            self.engine.read_rootlist();
            return None;
        }
        Some(wait)
    }

    fn background_work_since(&self) -> [Option<Instant>; 2] {
        let home = self.engine.home.loaded_at.filter(|_| self.engine.home.awaiting > 0);
        [
            self.engine.library
                .playlists_loading_since
                .into_iter()
                .chain(home)
                .min(),
            self.last_update_check.filter(|_| self.update_checking),
        ]
    }

    fn apply_actions(&mut self, ctx: &egui::Context) {
        for cover in self.engine.uploaded_covers.values() {
            ctx.include_bytes(cover.cover.uri.clone(), cover.cover.jpeg.clone());
        }
        self.engine.frame_now = None;
        let mut actions = std::mem::take(&mut self.engine.actions);
        while !actions.is_empty() {
            for action in actions.drain(..) {
                self.apply(action, ctx);
            }
            actions = std::mem::take(&mut self.engine.actions);
        }
    }

    fn leave_lyrics_fullscreen(&mut self, ctx: &egui::Context) {
        if let Some(was_fullscreen) = self.engine.lyrics_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(was_fullscreen));
            if self.engine.lyrics_restore_maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            self.engine.lyrics_fullscreen_restoring = Some(was_fullscreen);
            self.engine.lyrics_fullscreen_seen = false;
            self.engine.lyrics_line_shown = None;
        }
    }

    pub(crate) fn apply(&mut self, action: Action, ctx: &egui::Context) {
        if matches!(
            &action,
            Action::Open(_)
                | Action::OpenSongRadio { .. }
                | Action::OpenUri(_)
                | Action::OpenLink(_)
                | Action::FocusSearch
                | Action::Back
                | Action::Forward
                | Action::SignOut
                | Action::ToggleWinampWindow
                | Action::ToggleQueuePanel
        ) {
            self.leave_lyrics_fullscreen(ctx);
        }
        // Pausing or skipping while a play-search is in flight is a newer
        // choice; seeking and volume leave the search to play its match.
        if matches!(
            &action,
            Action::TogglePlay | Action::Next | Action::Previous
        ) {
            self.engine.supersede_play_search();
        }
        match action {
            Action::Open(page) => self.engine.open(page),
            Action::OpenSongRadio { uri, track } => self.engine.open_song_radio(&uri, &track),
            Action::PrepareTint(url) => {
                if self.engine.settings.accent_from_art {
                    self.engine.tint_for(Some(&url));
                }
            }
            Action::OpenUri(uri) => {
                if let Some(page) = Page::from_uri(&uri) {
                    self.engine.open(page);
                }
            }
            Action::OpenLink(uri) => {
                // The window first: a link is the user asking for the
                // app, signed in or not, and the page follows when it can.
                self.engine.prerelease_request = None;
                self.engine.pending_link = Some(uri);
                self.engine.open_pending_link();
                self.engine.actions.push(Action::ShowWindow);
            }
            Action::Back => {
                if self.engine.can_go_back() {
                    self.engine.pending_link = None;
                    self.engine.prerelease_request = None;
                    self.engine.history_index -= 1;
                    let page = self.engine.page().clone();
                    self.engine.touch_page(&page);
                    self.engine.ensure_loaded(page.clone());
                    self.engine.retain_table_rows(&page);
                    self.engine.evict_stale_pages();
                }
            }
            Action::Forward => {
                if self.engine.can_go_forward() {
                    self.engine.pending_link = None;
                    self.engine.prerelease_request = None;
                    self.engine.history_index += 1;
                    let page = self.engine.page().clone();
                    self.engine.touch_page(&page);
                    self.engine.ensure_loaded(page.clone());
                    self.engine.retain_table_rows(&page);
                    self.engine.evict_stale_pages();
                }
            }
            Action::PlayContext {
                uri,
                offset_uri,
                offset_index,
            } => {
                let mut request = PlayRequest::context(uri);
                request.offset_uri = offset_uri;
                request.offset_position = offset_index;
                self.engine.play_request(request, false);
            }
            // Answered by the search's response, which plays the match.
            // Taking the serial here, not when the command arrives, keeps
            // a play queued before it from superseding it.
            Action::PlaySearch { kind, query } => {
                self.engine.play_search_serial = self.engine.play_search_serial.wrapping_add(1);
                self.engine.backend.api(ApiRequest::PlaySearch {
                    kind,
                    query,
                    serial: self.engine.play_search_serial,
                });
            }
            Action::PlaySearchMatch { uri, serial } => {
                if serial == self.engine.play_search_serial {
                    self.engine.play_request(PlayRequest::context(uri), false);
                }
            }
            Action::PlayUris { uris, index } => {
                if uris.is_empty() {
                    return;
                }
                let (uris, index) = cap_uris(&uris, index);
                let request = PlayRequest::tracks(uris).starting_at_index(index);
                self.engine.play_request(request, false);
            }
            Action::PlayEpisode { uri, resume_ms } => {
                // A started episode continues from the place the row or card
                // showed as time left. The playing episode is left where it
                // is playing, not sent back to a saved position older than it.
                let mut request = PlayRequest::tracks(vec![uri.clone()]).starting_at_index(0);
                if self.engine.now_playing().is_none_or(|now| now.uri != uri)
                    && let Some(position_ms) = resume_ms
                {
                    request.position_ms = position_ms;
                }
                self.engine.play_request(request, false);
            }
            Action::PlayFromRow {
                context,
                uri,
                index,
            } => match context {
                RowContext::Context {
                    uri: context_uri, ..
                } => {
                    let request = PlayRequest::context(context_uri).starting_at_uri(uri);
                    self.engine.play_request(request, false);
                }
                RowContext::Uris(uris) => {
                    // The click names a song. A row that plays a list of its
                    // own, as each Recent row does, still hands over its
                    // place in the list on screen; when that place holds
                    // another song, the song wins, as it does in Next up.
                    let index = if uris.get(index as usize).is_some_and(|held| *held == uri) {
                        index
                    } else {
                        uris.iter()
                            .position(|held| *held == uri)
                            .map_or(index, |position| position as u32)
                    };
                    let (uris, index) = cap_uris(uris.as_ref(), index);
                    let request = PlayRequest::tracks(uris).starting_at_index(index);
                    self.engine.play_request(request, false);
                }
                RowContext::Queue => self.engine.play_queue_item(index as usize, uri),
                RowContext::View {
                    uris, context_uri, ..
                } => {
                    let (uris, index) = cap_uris(uris.as_ref(), index);
                    if let Some(uri) = uris.get(index as usize) {
                        self.engine.cache_track_from_context(&context_uri, uri);
                    }
                    let request = PlayRequest::tracks(uris).starting_at_index(index);
                    self.engine.play_request(request, false);
                    self.engine.note_recent_context(&context_uri);
                    self.engine.assumed_context = Some(AssumedContext {
                        uri: context_uri,
                        shuffle: self.engine.shuffle_wanted.then_some(true),
                        at: Instant::now(),
                    });
                }
            },
            Action::ShufflePlay(uri) => {
                // The random starting song is picked in `play_request`,
                // which every shuffled play goes through.
                self.engine.play_request(PlayRequest::context(uri), true);
            }
            Action::TogglePlay => self.engine.toggle_play(),
            Action::Next if self.engine.resume_only() => {
                self.engine.step_resume(true);
            }
            Action::Next => {
                // Move the queue head to the playing row immediately. Do not
                // pop when there is no active playback target.
                let target = self.engine.target();
                if !matches!(target, Target::Remote(None)) || self.engine.remote_fresh().is_some() {
                    self.engine.pop_queue_head();
                }
                match target {
                    Target::Local => self.engine.backend.player(PlayerCommand::Next),
                    Target::Remote(device_id) => self.engine.remote(RemoteAction::Next, device_id),
                }
            }
            // Previous restarts after three seconds and otherwise steps back,
            // matching librespot.
            Action::Previous if self.engine.resume_only() => {
                if self.engine.resume_position_ms > RESTART_BEFORE_PREVIOUS {
                    self.engine.resume_position_ms = 0;
                    self.engine.session_dirty = true;
                } else {
                    self.engine.step_resume(false);
                }
            }
            Action::Previous => {
                // Next names its expected destination so the row moves before
                // the engine does. Previous has no forward queue row to name;
                // discard that expectation or a quick Next, Previous leaves
                // the row marker stuck on the skipped-to song.
                self.engine.intent_track = None;
                self.engine.queue_start_pending = Some(self.engine.target());
                match self.engine.target() {
                    Target::Local => self.engine.backend.player(PlayerCommand::Previous),
                    Target::Remote(device_id) => self.engine.remote(RemoteAction::Previous, device_id),
                }
            }
            Action::Seek(position_ms) => self.engine.seek(position_ms),
            Action::SeekBy(offset) => {
                if let Some(now) = self.engine.now_playing() {
                    let target = (i64::from(now.position_ms) + offset)
                        .clamp(0, i64::from(now.duration_ms))
                        as u32;
                    self.engine.seek(target);
                }
            }
            Action::SetVolume(percent) => {
                self.engine.volume_before_mute = None;
                self.engine.set_volume(percent, true);
            }
            Action::PreviewVolume(percent) => self.engine.set_volume(percent, false),
            Action::VolumeBy(delta) => {
                if let Some(now) = self.engine.now_playing() {
                    let next =
                        (i16::from(now.volume_percent) + i16::from(delta)).clamp(0, 100) as u8;
                    self.engine.volume_before_mute = None;
                    self.engine.set_volume(next, true);
                } else if self.engine.is_connected() {
                    let current = volume_to_percent(self.engine.local.volume);
                    let next = (i16::from(current) + i16::from(delta)).clamp(0, 100) as u8;
                    self.engine.set_volume(next, true);
                }
            }
            Action::ToggleMute => {
                if !self.engine.can_set_volume() {
                    return;
                }
                let current = self.engine.now_playing()
                    .map(|now| now.volume_percent)
                    .unwrap_or_else(|| volume_to_percent(self.engine.local.volume));
                if current == 0 {
                    let restore = self.engine.volume_before_mute.take().unwrap_or(50).max(5);
                    self.engine.set_volume(restore, true);
                } else {
                    self.engine.volume_before_mute = Some(current);
                    self.engine.set_volume(0, true);
                }
            }
            Action::ToggleShuffle => {
                let shuffle = self.engine.now_playing()
                    .map_or(self.engine.shuffle_wanted, |now| now.shuffle);
                self.engine.set_shuffle(!shuffle);
            }
            Action::SetShuffle(shuffle) => self.engine.set_shuffle(shuffle),
            Action::CycleRepeat => {
                let mode = self.engine.now_playing().map(|now| now.repeat).unwrap_or_default();
                self.engine.set_repeat(mode.next());
            }
            Action::SetRepeat(mode) => self.engine.set_repeat(mode),
            Action::AddToQueue { uri, label } => self.engine.add_to_queue(uri, label),
            Action::QueueMany { songs } => self.engine.queue_many(songs),
            Action::MoveInQueue { from, to } => {
                if !self.engine.queue_locally_reorderable() {
                    return;
                }
                let queued_len = self.engine.queued_rows_len();
                if from >= queued_len || to > queued_len || from == to || to == from + 1 {
                    return;
                }
                if from < self.engine.manual_queue.len() {
                    let uri = self.engine.manual_queue.remove(from);
                    let at = (if to > from { to - 1 } else { to }).min(self.engine.manual_queue.len());
                    self.engine.manual_queue.insert(at, uri);
                    // Pending additions are tracked by manual_queue index;
                    // shift them the same way the move just shifted the row
                    // they point at, or they end up naming a different song.
                    for addition in &mut self.engine.pending_queue_adds {
                        if addition.manual_index == from {
                            addition.manual_index = at;
                        } else if from < addition.manual_index && addition.manual_index <= at {
                            addition.manual_index -= 1;
                        } else if at <= addition.manual_index && addition.manual_index < from {
                            addition.manual_index += 1;
                        }
                    }
                    self.engine.show_manual_queue_order(queued_len);
                }
                self.engine.session_dirty = true;
                self.engine.resync_local_queue();
            }
            Action::RemoveFromQueue { rows } => self.engine.remove_from_queue(rows),
            Action::InsertInQueue { items, position } => {
                if !self.engine.queue_locally_reorderable() {
                    let songs = items
                        .into_iter()
                        .map(|item| (item.uri().to_string(), item.name().to_string()))
                        .collect();
                    self.engine.queue_many(songs);
                    return;
                }
                let position = position.min(self.engine.queued_rows_len());
                let repeats: Vec<bool> = items
                    .iter()
                    .map(|item| self.engine.queued_moments_ago(item.uri()))
                    .collect();
                let mut inserted = 0;
                for (item, repeat) in items.into_iter().zip(repeats) {
                    if repeat {
                        continue;
                    }
                    let uri = item.uri().to_string();
                    let at = (position + inserted).min(self.engine.manual_queue.len());
                    // Inserting here shifts every later manual_queue row
                    // right by one; keep pending additions pointing at their
                    // own song rather than the one now sitting in their slot.
                    for addition in &mut self.engine.pending_queue_adds {
                        if addition.manual_index >= at {
                            addition.manual_index += 1;
                        }
                    }
                    self.engine.pending_queue_adds.push(PendingQueueAdd {
                        item: item.clone(),
                        at: Instant::now(),
                        manual_index: at,
                        write: None,
                    });
                    if let Loadable::Loaded(queue) = &mut self.engine.queue {
                        queue.queue.insert(at.min(queue.queue.len()), item);
                    }
                    self.engine.manual_queue.insert(at, uri);
                    inserted += 1;
                }
                if inserted > 0 {
                    self.engine.session_dirty = true;
                    self.engine.queued_toast(inserted);
                    self.engine.resync_local_queue();
                }
            }
            Action::SetSavedMany { uris, saved } => {
                let mut changed_tracks = false;
                for uri in &uris {
                    self.engine.set_saved_state(uri.clone(), saved);
                    self.engine.saved_writes.insert(uri.clone(), saved);
                    changed_tracks |= self.engine.change_liked_song(uri, saved);
                }
                if changed_tracks {
                    self.engine.sync_liked_songs();
                    self.engine.ensure_liked_songs();
                }
                if !uris.is_empty() {
                    self.engine.backend.api(ApiRequest::SetSaved { uris, saved });
                }
            }
            Action::ToggleSaved(uri) => {
                let saved = self.engine.is_saved(&uri).unwrap_or(false);
                let targets = if saved {
                    self.engine.saved_toggle_targets(&uri)
                } else {
                    vec![uri]
                };
                for target in targets {
                    self.engine.set_saved(target, !saved);
                }
            }
            Action::AddToPlaylist {
                playlist_id,
                playlist_name,
                items,
            } => {
                self.engine.request_playlist_add(playlist_id, playlist_name, items, None);
            }
            Action::InsertInPlaylist {
                playlist_id,
                position,
                items,
            } => {
                let name = self.engine.playlist_pages
                    .get(&playlist_id)
                    .and_then(|page| page.playlist.get())
                    .filter(|playlist| self.engine.can_edit_playlist(playlist))
                    .map(|playlist| playlist.name.clone());
                if let Some(name) = name {
                    self.engine.request_playlist_add(playlist_id, name, items, Some(position));
                }
            }
            Action::ConfirmAddToPlaylist {
                playlist_id,
                playlist_name,
                items,
                position,
            } => {
                self.engine.add_to_playlist_now(playlist_id, playlist_name, items, position);
            }
            Action::RemoveFromPlaylist { playlist_id, uris } => {
                let snapshot_id = self.engine.playlist_pages
                    .get(&playlist_id)
                    .and_then(|page| page.playlist.get())
                    .and_then(|playlist| playlist.snapshot_id.clone());
                self.engine.prepare_playlist_mutation(&playlist_id);
                if let Some(page) = self.engine.playlist_pages.get_mut(&playlist_id) {
                    page.local_additions
                        .retain(|uri| !uris.iter().any(|removed| removed == uri));
                    let before = page.items.items.len();
                    page.items.retain(|item| {
                        item.playable()
                            .is_none_or(|playable| !uris.iter().any(|uri| uri == playable.uri()))
                    });
                    let removed = (before - page.items.items.len()) as u32;
                    page.items.total = page.items.total.map(|total| total.saturating_sub(removed));
                    page.items.next_offset = page
                        .items
                        .next_offset
                        .map(|offset| offset.saturating_sub(removed));
                    if let Loadable::Loaded(playlist) = &mut page.playlist
                        && let Some(count) =
                            playlist.items_count.as_mut().or(playlist.tracks.as_mut())
                    {
                        count.total = count.total.saturating_sub(removed);
                    }
                }
                self.engine.playlist_busy = true;
                self.engine.backend.api(ApiRequest::RemoveFromPlaylist {
                    playlist_id,
                    uris,
                    snapshot_id,
                });
            }
            Action::MoveInPlaylist {
                playlist_id,
                from,
                to,
            } => {
                let snapshot_id = self.engine.playlist_pages
                    .get(&playlist_id)
                    .and_then(|page| page.playlist.get())
                    .and_then(|playlist| playlist.snapshot_id.clone());
                self.engine.prepare_playlist_mutation(&playlist_id);
                if let Some(page) = self.engine.playlist_pages.get_mut(&playlist_id) {
                    let base = page.items.base_offset;
                    if from >= base && to >= base {
                        page.items
                            .reorder((from - base) as usize, (to - base) as usize);
                    }
                }
                self.engine.playlist_busy = true;
                self.engine.backend.api(ApiRequest::ReorderPlaylist {
                    playlist_id,
                    range_start: from,
                    insert_before: to,
                    snapshot_id,
                });
            }
            Action::ChoosePlaylistCover(id) => {
                if self.engine.cover_uploads.contains_key(&id) {
                    return;
                }
                if let Some(Dialog::EditPlaylist {
                    id: current, cover, ..
                }) = &mut self.engine.dialog
                    && *current == id
                    && cover.request.is_none()
                    && cover.uploading.is_none()
                {
                    self.engine.cover_request = self.engine.cover_request.wrapping_add(1);
                    cover.request = Some(self.engine.cover_request);
                    cover.error = None;
                    self.engine.backend.choose_playlist_cover(id, self.engine.cover_request);
                }
            }
            Action::UploadPlaylistCover(id) => {
                if self.engine.cover_uploads.contains_key(&id) {
                    return;
                }
                let previous_urls = self.engine.uploaded_covers
                    .get(&id)
                    .map(|pending| pending.previous_urls.clone())
                    .unwrap_or_else(|| {
                        self.engine.playlist_pages
                            .get(&id)
                            .and_then(|page| page.playlist.get())
                            .into_iter()
                            .chain(
                                self.engine.library
                                    .playlists
                                    .get()
                                    .into_iter()
                                    .flatten()
                                    .filter(|playlist| playlist.id == id),
                            )
                            .flat_map(|playlist| {
                                playlist.images.iter().map(|image| image.url.clone())
                            })
                            .collect()
                    });
                if let Some(Dialog::EditPlaylist {
                    id: current, cover, ..
                }) = &mut self.engine.dialog
                    && *current == id
                    && cover.uploading.is_none()
                    && cover.request.is_none()
                    && let Some(selected) = cover.selection.clone()
                {
                    self.engine.cover_request = self.engine.cover_request.wrapping_add(1);
                    cover.uploading = Some(self.engine.cover_request);
                    self.engine.cover_uploads.insert(id.clone(), self.engine.cover_request);
                    cover.error = None;
                    self.engine.backend.api(ApiRequest::UploadPlaylistCover {
                        id,
                        request: self.engine.cover_request,
                        previous_urls,
                        cover: selected,
                    });
                }
            }
            Action::ShowDialog(mut dialog) => {
                if let Dialog::EditPlaylist { id, cover, .. } = &mut dialog
                    && let Some(request) = self.engine.cover_uploads.get(id)
                {
                    cover.uploading = Some(*request);
                }
                self.engine.dialog = Some(dialog);
            }
            Action::CloseDialog => {
                if matches!(self.engine.dialog, Some(Dialog::PersonalAppIntro)) {
                    self.engine.settings.personal_app_intro_seen = true;
                    self.engine.settings_dirty = true;
                }
                self.engine.dialog = None;
            }
            Action::CreatePlaylist {
                name,
                public,
                add_uris,
            } => {
                self.engine.playlist_busy = true;
                self.engine.dialog = Some(Dialog::CreatePlaylist {
                    name: name.clone(),
                    public,
                    add_uris,
                });
                self.engine.backend.api(ApiRequest::CreatePlaylist {
                    name,
                    public,
                    description: String::new(),
                });
            }
            Action::UpdatePlaylist {
                id,
                name,
                description,
                public,
            } => {
                self.engine.dialog = None;
                let changes = self.engine.changed_playlist_details(&id, name, description, public);
                if changes.kept_description {
                    self.engine.toast_error(self.engine.localize.t(
                        self.engine.locale,
                        "Spotify doesn't let apps remove a playlist description, so it was kept",
                    ));
                }
                if changes.name.is_some()
                    || changes.description.is_some()
                    || changes.public.is_some()
                {
                    self.engine.playlist_busy = true;
                    self.engine.backend.api(ApiRequest::UpdatePlaylist {
                        id,
                        name: changes.name,
                        description: changes.description,
                        public: changes.public,
                    });
                }
            }
            Action::DeletePlaylist(id) => {
                self.engine.dialog = None;
                let uri = format!("spotify:playlist:{id}");
                self.engine.saved.insert(uri.clone(), false);
                self.engine.edit_playlists(PlaylistEdit {
                    uri,
                    added: None,
                    settled_by: None,
                });
                self.engine.backend
                    .api(ApiRequest::FollowPlaylist { id, follow: false });
            }
            Action::Transfer(device_id) => self.engine.transfer(device_id),
            Action::ActivateReceiver(receiver) => {
                if self.engine.activating_receiver.is_none() {
                    self.engine.activating_receiver = Some(receiver.name.clone());
                    self.engine.activating_play = self.engine.now_playing().is_some_and(|now| now.playing);
                    self.engine.backend.send(Command::ActivateReceiver(receiver));
                }
            }
            Action::RefreshDevices => {
                self.engine.devices_fetched_at = None;
                self.engine.refresh_devices();
                self.engine.backend.send(Command::DiscoverReceivers);
            }
            Action::ClearQueue => self.engine.clear_queue(),
            Action::SaveQueueAsPlaylist => self.engine.save_queue_as_playlist(),
            Action::SaveRadio(seed) => self.engine.save_radio(&seed),
            Action::RefreshQueue => self.engine.refresh_queue(true),
            Action::CopyLink(uri) => {
                if let Some(url) = util::open_spotify_url(&uri) {
                    ctx.copy_text(url);
                    self.engine.toast(self.engine.localize.t(self.engine.locale, "Link copied"));
                }
            }
            Action::CopySongs(items) => {
                let links: Vec<String> = items
                    .iter()
                    .filter_map(|item| util::open_spotify_url(item.uri()))
                    .collect();
                if !links.is_empty() {
                    // One link a line, in the platform's own line breaks.
                    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
                    ctx.copy_text(links.join(newline));
                    self.engine.toast(match links.len() {
                        1 => self.engine.localize.t(self.engine.locale, "Link copied").into_owned(),
                        count => self.engine.localize.tn(
                            self.engine.locale,
                            // Translators: {count} is a number of song links, always more than one.
                            "{count} link copied",
                            "{count} links copied",
                            count as u32,
                        )
                        .replace("{count}", &count.to_string()),
                    });
                    self.engine.copied_songs = items;
                }
            }
            Action::PasteSongs { playlist_id, text } => self.engine.paste_songs(playlist_id, &text),
            Action::OpenInSpotify(uri) => {
                if let Some(url) = util::open_spotify_url(&uri) {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
            Action::Search(query) => {
                // Enter submits what was typed; a recent search or a link
                // starts afresh.
                let typed = query == self.engine.search.query;
                self.engine.search.query = query.clone();
                self.engine.search.typed_at = None;
                self.engine.open(Page::Search);
                if typed {
                    self.engine.submit_typed_search(query.trim().to_string());
                } else {
                    self.engine.search.edit_run += 1;
                    self.engine.run_search(query.trim().to_string());
                }
            }
            Action::EndSearchRun => self.engine.search.edit_run += 1,
            Action::ForgetSearch(query) => {
                self.engine.settings.search_history.retain(|entry| entry != &query);
                self.engine.settings_dirty = true;
            }
            Action::SetSearchFilter(filter) => self.engine.search.filter = filter,
            Action::FocusSearch => {
                self.engine.search.focus_requested = true;
                if !matches!(self.engine.page(), Page::Search) {
                    self.engine.open(Page::Search);
                }
            }
            Action::LoadMore(page) => self.engine.load_more(page),
            Action::LoadWindow { page, position } => self.engine.load_window(page, position),
            Action::RetryWindow(page) => self.engine.retry_window(page),
            Action::LoadMoreRecents => self.engine.load_more_recents(),
            Action::ReloadRecents => self.engine.reload_recents(),
            Action::SetQueueTab(tab) => {
                self.engine.queue_tab = tab;
                self.engine.session_dirty = true;
                if tab == QueueTab::Recents
                    && self.engine.recents.items.is_empty()
                    && !self.engine.recents.loading
                {
                    self.engine.load_recents(false);
                }
            }
            Action::LoadMoreArtistAlbums(id) => {
                let Some(page) = self.engine.artist_pages.get_mut(&id) else {
                    return;
                };
                let groups = page.filter.groups().to_string();
                let list = page.albums.entry(groups.clone()).or_default();
                if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                    list.loading = true;
                    self.engine.backend
                        .api(ApiRequest::ArtistAlbums { id, groups, offset });
                }
            }
            Action::SetDiscographyFilter { artist_id, filter } => {
                if let Some(page) = self.engine.artist_pages.get_mut(&artist_id) {
                    page.filter = filter;
                }
                self.engine.load_artist_albums(&artist_id, filter);
            }
            Action::ToggleShowAllTop(id) => {
                if let Some(page) = self.engine.artist_pages.get_mut(&id) {
                    page.show_all_top = !page.show_all_top;
                }
            }
            Action::Reload(page) => self.engine.reload(page),
            Action::SignIn => self.request_proxy(true),
            Action::ApplyProxy => self.request_proxy(false),
            Action::ProxyEdited => self.engine.proxy_form_edited = true,
            Action::CancelSignIn => {
                self.engine.backend.send(Command::CancelSignIn);
                self.engine.sign_in_url = None;
                self.engine.auth = AuthStatus::SignedOut;
            }
            Action::ConfigurePersonalWebApp => {
                self.save_settings();
                self.engine.backend.send(Command::ConfigurePersonalWebApp(
                    self.engine.settings.web_client_id.clone(),
                ));
            }
            Action::OpenPersonalAppSetup => {
                self.engine.settings.personal_app_intro_seen = true;
                self.engine.settings_dirty = true;
                self.engine.dialog = None;
                self.engine.open(Page::Settings);
                // A saved search could be hiding the Client ID field the
                // flow is about to focus, so drop it before landing.
                crate::ui::settings::clear_search(ctx);
                ctx.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new(crate::ui::settings::PERSONAL_APP_FOCUS_ID),
                        true,
                    );
                });
            }
            Action::SignOut => {
                self.engine.backend.send(Command::SignOut);
                self.engine.history = vec![Page::Home];
                self.engine.history_index = 0;
            }
            Action::ToggleSidebar => {
                self.engine.settings.sidebar_visible = !self.engine.settings.sidebar_visible;
                self.engine.settings_dirty = true;
            }
            Action::ToggleQueuePanel => {
                self.engine.show_queue_panel = !self.engine.show_queue_panel;
                if self.engine.show_queue_panel {
                    self.engine.show_lyrics_panel = false;
                    self.engine.refresh_queue(true);
                }
            }
            Action::ToggleLyricsPanel => {
                self.leave_lyrics_fullscreen(ctx);
                self.engine.show_lyrics_panel = !self.engine.show_lyrics_panel;
                if self.engine.show_lyrics_panel {
                    self.engine.show_queue_panel = false;
                    self.engine.lyrics_following = true;
                    self.request_lyrics();
                }
            }
            Action::SetLyricsFullscreen(fullscreen) => {
                if fullscreen && self.engine.lyrics_fullscreen.is_none() {
                    self.engine.lyrics_fullscreen =
                        Some(self.engine.lyrics_fullscreen_restoring.take().unwrap_or_else(|| {
                            ctx.input(|input| input.viewport().fullscreen.unwrap_or(false))
                        }));
                    #[cfg(windows)]
                    {
                        // Winit's undecorated maximized Windows client area is
                        // constrained to the work area, even in fullscreen.
                        // Clear maximization before entering, then restore it
                        // together with the original window mode on exit.
                        self.engine.lyrics_restore_maximized |= self.engine.lyrics_fullscreen == Some(false)
                            && ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                        if self.engine.lyrics_restore_maximized {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                        }
                    }
                    self.engine.lyrics_fullscreen_seen = false;
                    self.engine.show_lyrics_panel = true;
                    self.engine.show_queue_panel = false;
                    self.engine.lyrics_following = true;
                    self.engine.lyrics_line_shown = None;
                    self.request_lyrics();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                } else if !fullscreen {
                    self.leave_lyrics_fullscreen(ctx);
                }
            }
            Action::LyricsLineShown(line) => {
                // Escape or navigation may already have left the view earlier
                // in this frame. The returning panel still needs to reposition.
                if self.engine.lyrics_fullscreen.is_some() {
                    self.engine.lyrics_line_shown = Some(line);
                }
            }
            Action::FollowLyrics => {
                self.engine.lyrics_following = true;
                self.engine.lyrics_line_shown = None;
            }
            Action::PauseLyricsFollow => self.engine.lyrics_following = false,
            Action::RetryLyrics => self.request_lyrics(),
            Action::ToggleDevicesPopup => {
                self.engine.show_devices = !self.engine.show_devices;
                if self.engine.show_devices {
                    self.engine.refresh_devices();
                    // Receivers waiting on the network are invisible to the
                    // Web API, so look for them ourselves.
                    self.engine.backend.send(Command::DiscoverReceivers);
                }
            }
            Action::CheckForUpdates => self.check_for_updates(true),
            Action::ShowUpdate => {
                self.show_update = true;
                if self.update_support.is_none() {
                    self.engine.backend.send(Command::InspectUpdate);
                }
            }
            Action::DownloadUpdate => {
                if matches!(
                    self.update_download,
                    crate::updates::DownloadState::Idle | crate::updates::DownloadState::Failed(_)
                ) && let Some(release) = self.update.clone()
                {
                    self.update_download = crate::updates::DownloadState::Downloading {
                        received: 0,
                        total: 0,
                    };
                    self.engine.backend.send(Command::DownloadUpdate {
                        release,
                        source: self.update_source.clone(),
                    });
                }
            }
            Action::InstallUpdate => {
                if matches!(
                    self.update_download,
                    crate::updates::DownloadState::Ready(_)
                ) && let crate::updates::DownloadState::Ready(prepared) = std::mem::replace(
                    &mut self.update_download,
                    crate::updates::DownloadState::Installing,
                ) {
                    self.engine.backend.send(Command::InstallUpdate {
                        prepared,
                        arguments: self.update_restart_arguments.clone(),
                    });
                }
            }
            Action::SetLibraryByYou(by_you) => {
                self.engine.settings.library_by_you = by_you;
                self.engine.mark_settings_dirty();
            }
            Action::SetLibrarySort { shelf, sort } => {
                if sort.supports(shelf) {
                    self.engine.settings.library_sort.insert(shelf, sort);
                    match shelf {
                        crate::settings::LibraryShelf::Albums => self.engine.library.albums.error = None,
                        crate::settings::LibraryShelf::Artists => self.engine.library.artists.error = None,
                        crate::settings::LibraryShelf::Podcasts => self.engine.library.shows.error = None,
                        crate::settings::LibraryShelf::Playlists => {}
                    }
                    self.engine.mark_settings_dirty();
                }
            }
            Action::SetLibraryGrid(grid) => {
                self.engine.settings.sidebar_grid = grid;
                self.engine.mark_settings_dirty();
            }
            Action::ToggleLibraryFolder(id) => {
                if self.engine.collapsed_folders.contains(&id) {
                    self.engine.collapsed_folders.retain(|held| held != &id);
                } else {
                    self.engine.collapsed_folders.push(id);
                }
                self.engine.session_dirty = true;
            }
            Action::ArrangeLibrary {
                pinned,
                playlist_order,
            } => {
                self.engine.settings.liked_songs_pinned = pinned
                    .iter()
                    .any(|key| key == crate::settings::LIKED_SONGS_KEY);
                self.engine.settings.pinned_contexts = pinned;
                if let Some(order) = playlist_order {
                    // Spotify doesn't let apps change the playlist order it
                    // keeps, so a drag in that order starts a local one.
                    if crate::ui::sidebar::selected_sort(
                        self,
                        crate::settings::LibraryShelf::Playlists,
                    ) == crate::settings::LibrarySort::Spotify
                    {
                        self.engine.toast(self.engine.localize.t(
                            self.engine.locale,
                            "Spotify doesn't let apps reorder your playlists, so this order is saved on this computer",
                        ));
                    }
                    self.engine.settings.sidebar_order = order;
                    self.engine.settings.library_sort.insert(
                        crate::settings::LibraryShelf::Playlists,
                        crate::settings::LibrarySort::Local,
                    );
                }
                self.engine.settings
                    .sidebar_order
                    .retain(|key| !self.engine.settings.pinned_contexts.contains(key));
                self.engine.mark_settings_dirty();
            }
            Action::SetTheme(choice) => {
                self.engine.settings.theme = choice;
                self.engine.settings.custom_theme = None;
                self.engine.settings.custom_theme_cache = None;
                self.engine.mark_settings_dirty();
                ctx.set_theme(self.theme_preference());
                self.apply_theme(ctx);
            }
            Action::SetLanguage(choice) => {
                self.engine.settings.language = choice;
                self.engine.locale = choice.resolve();
                self.engine.mark_settings_dirty();
                ctx.request_repaint();
            }
            Action::SetCustomTheme(filename) => {
                if let Some(theme) = self.custom_themes.find(&filename) {
                    self.engine.settings.custom_theme_cache = serde_json::to_value(theme).ok();
                    self.engine.settings.custom_theme = Some(filename);
                    self.engine.mark_settings_dirty();
                    ctx.set_theme(self.theme_preference());
                    self.apply_theme(ctx);
                }
            }
            Action::ReloadThemes => {
                let waker = Waker::default();
                waker.attach(ctx);
                self.load_custom_themes(&waker);
            }
            Action::OpenThemesFolder => {
                self.engine.backend.send(Command::OpenThemesFolder);
            }
            Action::SettingsChanged => {
                self.engine.settings_dirty = true;
                ctx.set_theme(self.theme_preference());
            }
            Action::RestartEngine => {
                self.save_settings();
                let config = engine_config(
                    &self.engine.dirs,
                    &self.engine.settings,
                    self.engine.applied_proxy.clone(),
                    std::sync::Arc::clone(&self.winamp.tap),
                    std::sync::Arc::clone(&self.winamp.eq),
                );
                self.engine.backend.send(Command::RestartEngine(config));
                if self.engine.local_ready {
                    self.engine.toast(self.engine.localize.t(self.engine.locale, "Restarting local playback"));
                }
            }
            Action::ShowWindow => {
                if self.window_hidden {
                    // No window exists; the outer loop creates one.
                    self.wants_show = true;
                } else if fastframe_shell::window::raise_on_compositor() {
                    // Wayland drops a window's own request for focus; niri
                    // focuses it on request, switching to its workspace.
                } else {
                    // A minimized window has to be restored before it can be
                    // focused: Focus alone leaves it in the Dock, and the
                    // platform draws no frames while it is down there, so
                    // nothing arrives to ask a second time.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            }
            Action::HideWindow => {
                if self
                    .tray
                    .as_ref()
                    .is_some_and(fastframe_tray::Tray::is_shown)
                {
                    self.hide_intent = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Action::EnablePlayback => {
                let free = self.engine.user
                    .as_ref()
                    .and_then(|user| user.product.as_deref())
                    .is_some_and(|product| product != "premium");
                if free {
                    self.engine.toast_error(self.engine.localize.t(self.engine.locale, "Local playback needs Spotify Premium"));
                } else if !self.engine.local_ready
                    && !matches!(
                        self.engine.local_playback,
                        LocalPlayback::Authorizing
                            | LocalPlayback::Connecting
                            | LocalPlayback::WaitingForNetwork
                    )
                {
                    self.engine.settings.playback_authorized = true;
                    self.engine.settings_dirty = true;
                    // The backend tries a saved grant first; the browser
                    // toast follows `LocalPlayback::Authorizing` (#818).
                    self.engine.backend.send(Command::AuthorizePlayback);
                }
            }
            Action::SetPrereleaseSaved { uri, saved } => {
                if self.engine.local_ready {
                    for page in self.engine.album_pages.values_mut() {
                        if let Some(data) = &mut page.prerelease
                            && data.uri == uri
                            && data.saved.is_some()
                            && !data.saving
                        {
                            data.saved = Some(saved);
                            data.saving = true;
                            data.save_error = None;
                            self.engine.backend.send(Command::SetPrereleaseSaved {
                                uri: data.album.uri.clone(),
                                saved,
                                generation: page.generation,
                            });
                            break;
                        }
                    }
                }
            }
            Action::OpenUrl(url) => {
                // Open links off the UI thread with the platform-specific
                // launcher used elsewhere in the app.
                std::thread::spawn(move || {
                    if let Err(error) = crate::opener::open(&url) {
                        log::warn!("unable to open {url}: {error}");
                    }
                });
            }
            Action::ClearPlayHistory => {
                self.engine.plays.clear();
                if let Some(bytes) = self.engine.plays.take_serialized() { self.engine.files.write_private(&self.engine.dirs.history_file(), bytes); }
                self.engine.rebuild_recents();
                self.engine.toast(self.engine.localize.t(self.engine.locale, "Play history cleared"));
            }
            Action::ClearArtCache => match self.art.clear_disk_cache() {
                Ok(bytes) => {
                    ctx.forget_all_images();
                    // The media controls hold a path into what was just
                    // deleted; forget it, or the next sync hands the system
                    // a file that is no longer there.
                    self.media_art = None;
                    self.engine.toast(
                        // Translators: {size} is a size in megabytes, such as 12.5.
                        self.engine.localize.t(self.engine.locale, "Cleared {size} MB of artwork")
                            .replace("{size}", &format!("{:.1}", bytes as f64 / 1_048_576.0)),
                    );
                }
                Err(error) => self.engine.toast_error(
                    // Translators: {error} is an error message.
                    self.engine.localize.t(self.engine.locale, "Couldn't clear artwork: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            Action::ToggleWinampWindow => {
                // One window at a time: this one closes and the loop in
                // `main` opens the other kind where each was last.
                if !self.engine.settings.winamp_window && self.engine.settings.random_skin {
                    self.winamp.refresh_choices(&self.engine.dirs.skins_dir());
                    let candidates: Vec<Option<String>> = std::iter::once(None)
                        .chain(
                            self.winamp
                                .choices
                                .iter()
                                .map(|choice| Some(choice.name.clone())),
                        )
                        .collect();
                    self.engine.settings.skin = crate::winamp::pick_another(
                        &candidates,
                        &self.engine.settings.skin,
                        &mut rand::rng(),
                    );
                }
                self.engine.settings.winamp_window = !self.engine.settings.winamp_window;
                self.engine.settings_dirty = true;
                self.switch_intent = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::SetSkin(name) => {
                self.engine.settings.skin = name;
                self.engine.settings.random_skin = false;
                self.engine.settings_dirty = true;
            }
            Action::SetRandomSkin(random) => {
                self.engine.settings.random_skin = random;
                self.engine.settings_dirty = true;
            }
            Action::InstallSkin(path) => {
                self.winamp.install(path, &self.engine.dirs.skins_dir(), ctx);
            }
            Action::SetSkinScale(scale) => {
                self.engine.settings.skin_scale = Some(scale);
                self.engine.settings_dirty = true;
            }
            Action::ToggleWinampOnTop => {
                if self.window_level_supported {
                    self.engine.settings.winamp_on_top = !self.engine.settings.winamp_on_top;
                    self.engine.settings_dirty = true;
                    self.push_winamp_level(ctx);
                }
            }
            Action::SetCustomTitlebar(custom) => {
                if self.engine.settings.custom_titlebar != custom {
                    self.engine.settings.custom_titlebar = custom;
                    fastframe_titlebar::set_enabled(custom);
                    self.engine.mark_settings_dirty();
                    if !self.engine.settings.winamp_window {
                        // Decorations are fixed at creation. Replace only the
                        // native window, keeping the page and playback.
                        self.switch_intent = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            Action::SetWinampTaskbar(visible) => {
                if self.engine.settings.winamp_show_taskbar != visible {
                    self.engine.settings.winamp_show_taskbar = visible;
                    self.engine.mark_settings_dirty();
                    if self.engine.settings.winamp_window {
                        // This window attribute is fixed at creation. Keep
                        // the visible mini player, its position, and playback
                        // while replacing only its native window.
                        self.switch_intent = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            Action::ToggleWinampPlaylist => {
                self.engine.settings.playlist_open = !self.engine.settings.playlist_open;
                self.engine.settings_dirty = true;
                if self.engine.settings.playlist_open {
                    self.engine.refresh_queue(false);
                }
            }
            Action::SetPlaylistHeight(height) => {
                self.engine.settings.playlist_height = height.clamp(
                    crate::skin::layout::PLAYLIST_MIN_HEIGHT,
                    crate::skin::layout::PLAYLIST_MAX_HEIGHT,
                );
                self.engine.settings_dirty = true;
            }
            Action::ToggleWinampEq => {
                self.engine.settings.eq_open = !self.engine.settings.eq_open;
                self.engine.settings_dirty = true;
            }
            Action::ToggleEq => {
                self.engine.settings.eq_on = !self.engine.settings.eq_on;
                self.push_eq();
            }
            Action::SetEqBand(band, gain_db) => {
                if let Some(slot) = self.engine.settings.eq_bands_db.get_mut(band) {
                    *slot = gain_db.clamp(-crate::eq::RANGE_DB, crate::eq::RANGE_DB);
                    self.push_eq();
                }
            }
            Action::SetEqPreamp(gain_db) => {
                self.engine.settings.eq_preamp_db =
                    gain_db.clamp(-crate::eq::RANGE_DB, crate::eq::RANGE_DB);
                self.push_eq();
            }
            Action::ApplyEqPreset(index) => {
                if let Some(preset) = crate::eq::PRESETS.get(index) {
                    self.engine.settings.eq_bands_db = preset.bands_db;
                    self.engine.settings.eq_on = true;
                    self.push_eq();
                }
            }
            Action::SetBalance(balance) => {
                self.engine.settings.balance = balance.clamp(-1.0, 1.0);
                self.push_eq();
            }
            Action::ToggleMono => {
                self.engine.settings.mono = !self.engine.settings.mono;
                self.push_eq();
            }
            Action::ToggleWinampShade => {
                self.engine.settings.winamp_shaded = !self.engine.settings.winamp_shaded;
                self.engine.settings_dirty = true;
            }
            Action::ToggleWinampPlaylistShade => {
                self.engine.settings.playlist_shaded = !self.engine.settings.playlist_shaded;
                self.engine.settings_dirty = true;
            }
            Action::ToggleWinampEqShade => {
                self.engine.settings.eq_shaded = !self.engine.settings.eq_shaded;
                self.engine.settings_dirty = true;
            }
            // The same request the window's own close button makes, so the
            // close-to-tray setting decides what follows.
            Action::CloseWindow => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::CyclePlayerBarVis => {
                self.engine.settings.player_bar_vis = self.engine.settings.player_bar_vis.next();
                self.engine.settings_dirty = true;
            }
            Action::CycleVisualiser => {
                self.engine.settings.vis = self.engine.settings.vis.next();
                self.engine.settings_dirty = true;
                self.winamp.analyser.reset();
            }
            Action::SetVisualiser(mode) => {
                if self.engine.settings.vis != mode {
                    self.engine.settings.vis = mode;
                    self.engine.settings_dirty = true;
                    self.winamp.analyser.reset();
                }
            }
            Action::OpenSkinsFolder => self.open_folder(self.engine.dirs.skins_dir()),
            Action::ToggleWinampMilkdrop => {
                self.engine.settings.milkdrop_open = !self.engine.settings.milkdrop_open;
                self.engine.settings_dirty = true;
                #[cfg(feature = "milkdrop")]
                if self.engine.settings.milkdrop_open {
                    // A first open has nothing to draw but the idle preset,
                    // which hardly answers the music; fetch the packs in the
                    // background and the window fills up on its own.
                    let folder = self.engine.dirs.milkdrop_dir();
                    if self.winamp.presets.downloading().is_none()
                        && !crate::milkdrop::has_presets(&folder)
                    {
                        self.winamp.presets.download_missing(
                            folder,
                            ctx.clone(),
                            self.engine.applied_proxy.clone(),
                        );
                        self.engine.toast(self.engine.localize.t(self.engine.locale, "Downloading MilkDrop preset packs"));
                    }
                }
            }
            Action::SetMilkdropSeconds(seconds) => {
                self.engine.settings.milkdrop_seconds = seconds.clamp(1, 3600);
                self.engine.settings_dirty = true;
            }
            Action::SetMilkdropFps(fps) => {
                self.engine.settings.milkdrop_fps = if fps == 0 {
                    0
                } else {
                    fps.clamp(
                        *crate::milkdrop::FPS_RANGE.start(),
                        *crate::milkdrop::FPS_RANGE.end(),
                    )
                };
                self.engine.settings_dirty = true;
            }
            Action::SetMilkdropScale(scale) => {
                self.engine.settings.milkdrop_scale = scale.clamp(1, 4);
                self.engine.settings_dirty = true;
            }
            Action::OpenMilkdropFolder => self.open_folder(self.engine.dirs.milkdrop_dir()),
            Action::DownloadMilkdropPack(index) => {
                if let Some(pack) = crate::milkdrop::PACKS.get(index) {
                    self.winamp.presets.download(
                        pack,
                        self.engine.dirs.milkdrop_dir(),
                        ctx.clone(),
                        self.engine.applied_proxy.clone(),
                    );
                    self.engine.toast(
                        // Translators: {name} is the name of a visualizer preset pack.
                        self.engine.localize.t(self.engine.locale, "Downloading {name} presets")
                            .replace("{name}", pack.name),
                    );
                }
            }
            Action::Quit => {
                self.engine.quit_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn check_for_updates(&mut self, manual: bool) {
        if self.update_checking
            || (self.engine.offline && self.update_source.is_github())
            || !matches!(
                self.update_download,
                crate::updates::DownloadState::Idle | crate::updates::DownloadState::Failed(_)
            )
        {
            return;
        }
        self.update_checking = true;
        self.last_update_check = Some(Instant::now());
        self.engine.backend.send(Command::CheckForUpdates {
            manual,
            source: self.update_source.clone(),
        });
    }

    pub fn background_frame(&mut self, ctx: &egui::Context) {
        if self.autoscroll.cancel_if_unfocused(ctx) {
            self.scrolling.stop();
        }
        self.handle_control_commands();
        self.handle_events();
        self.engine.open_pending_link();
        self.handle_media_commands();
        self.sync_power(ctx);
        self.sync_motion(ctx);
        self.handle_tray();
        #[cfg(target_os = "macos")]
        self.engine.handle_dock_menu();
        #[cfg(target_os = "macos")]
        self.engine.handle_notch_commands();
        self.tick(ctx);
        self.engine.note_listening();
        // MilkDrop runs in a child process and can outlive the main window.
        // Poll it before applying actions because its keys produce actions.
        #[cfg(feature = "milkdrop")]
        self.sync_milkdrop(ctx);
        self.apply_actions(ctx);
        self.sync_media_controls(ctx);
        self.sync_window_title(ctx);
        self.remember_geometry(ctx);
        #[cfg(target_os = "macos")]
        self.sync_notch_widget(ctx);
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        self.note_close_request(close_requested, self.hides_to_tray());
        self.schedule_next_pass(ctx);
    }

    fn note_close_request(&mut self, close_requested: bool, hides_to_tray: bool) {
        if close_requested && !self.engine.quit_requested && !self.switch_intent && hides_to_tray {
            self.hide_intent = true;
        }
    }

    fn remember_geometry(&mut self, ctx: &egui::Context) {
        if self.window_hidden || self.switch_intent {
            return;
        }
        if self.engine.settings.winamp_window {
            self.winamp.geometry.remember(ctx);
        } else if self.engine.lyrics_fullscreen.is_none() && self.engine.lyrics_fullscreen_restoring.is_none() {
            self.window_geometry.remember(ctx);
        }
    }

    fn metered(&self) -> bool {
        self.power.conditions().metered
    }

    fn downloads_wait(&self) -> bool {
        let conditions = self.power.conditions();
        conditions.metered || (self.follows_reachability && !conditions.online)
    }

    fn take_update_step(&mut self, step: HeldUpdate) {
        if self.downloads_wait() {
            self.held_update = Some(step);
            return;
        }
        match step {
            HeldUpdate::Inspect => self.engine.backend.send(Command::InspectUpdate),
            HeldUpdate::Download => self.engine.actions.push(Action::DownloadUpdate),
        }
    }

    fn sync_motion(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "linux")]
        let desktop = self
            .system_appearance
            .as_ref()
            .and_then(crate::appearance::SystemAppearance::animations);
        #[cfg(not(target_os = "linux"))]
        let desktop = None;
        if self.motion.refresh(Instant::now(), desktop) {
            log::debug!("reduce motion: {}", self.motion.reduced());
            crate::motion::apply(ctx, self.motion.reduced());
        }
    }

    fn sync_power(&mut self, ctx: &egui::Context) {
        let conditions = self.power.conditions();
        let shown =
            !self.window_hidden && !ctx.input(|input| input.viewport().minimized.unwrap_or(false));
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
        let budget = crate::power::budget(conditions, shown, focused);
        if budget != self.budget {
            log::debug!("budget: {budget:?}");
            self.budget = budget;
            self.power
                .set_background(budget == crate::power::Budget::Background);
        }
        if self.follows_reachability && conditions.online != self.reachable {
            self.reachable = conditions.online;
            self.engine.backend.network_status(conditions.online);
        }
        if crate::power::playback_needs_activity(&self.engine.local) {
            self.playback_activity
                .get_or_insert_with(|| crate::power::Activity::begin("Spotify playback"));
        } else {
            self.playback_activity = None;
        }
        for event in self.power.events() {
            use crate::power::Event;
            match event {
                Event::Suspending(ack) | Event::EndingSession(ack) => {
                    self.save_state();
                    if let Some(bytes) = self.engine.plays.take_serialized() { self.engine.files.write_private(&self.engine.dirs.history_file(), bytes); }
                    // The system may stop for good after this: the writes
                    // reach the disk before it is told to go ahead, within
                    // the time the system is held for.
                    self.engine.files.flush(SUSPEND_FLUSH_TIMEOUT);
                    ack.done();
                }
                Event::Resumed => {}
            }
        }
    }

    fn schedule_next_pass(&self, ctx: &egui::Context) {
        if let Some(due) = self.next_pass() {
            ctx.request_repaint_after(due);
        }
    }

    fn headless_wait(&self) -> fastframe_shell::Headless {
        use fastframe_shell::{HEADLESS_TICK, Headless};
        if self.engine.settings.milkdrop_open {
            return Headless::Wait;
        }
        let wait = self
            .next_pass()
            .map_or(HEADLESS_SLEEP, |due| due.min(HEADLESS_SLEEP));
        if wait <= HEADLESS_TICK {
            Headless::Wait
        } else {
            Headless::WaitFor(wait)
        }
    }

    #[allow(dead_code)]
    fn sync_notch_widget(&mut self, _ctx: &egui::Context) {
        let is_background = _ctx.input(|i| i.viewport().minimized.unwrap_or(false))
            || self.window_hidden
            || !_ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let enabled = self.engine.settings.mac_notch_widget;
        let active = crate::notch::is_active();
        let now_opt = self.engine.now_playing();

        let track_info = now_opt.map(|now| {
            let art_path = now
                .art_url
                .as_deref()
                .or(now.art_small.as_deref())
                .and_then(|url| self.media_art_file(_ctx, url));
            let artist = now
                .artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let saved = self.engine.is_saved(&now.uri).unwrap_or(false);
            let tint = self.now_playing_tint().unwrap_or(self.palette.accent);
            let accent = Some([tint.r(), tint.g(), tint.b()]);
            crate::notch::NotchTrackInfo {
                playing: now.playing,
                title: now.title,
                artist,
                album: now.album_name,
                duration_ms: now.duration_ms,
                position_ms: now.position_ms,
                art_path,
                uri: now.uri,
                saved,
                accent,
                is_episode: now.is_episode,
                is_remote: !now.local,
                shuffle: now.shuffle,
                repeat: now.repeat,
            }
        });
        crate::notch::sync_state(enabled, is_background, track_info.as_ref());
        // Only schedule a fast repaint while the card is actively expanded or
        // animating. Hover and state-change callbacks already call notch::wake()
        // which triggers a repaint via the waker, so idle frames are not needed.
        if enabled && active {
            _ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    pub fn frame_ui(&mut self, ui: &mut egui::Ui) {
        // The native close can take another frame. Settings already describe
        // the replacement window, but drawing it here would resize this one
        // before eframe saves its geometry. attach clears the switch intent
        // only once the replacement exists.
        if self.switch_intent {
            return;
        }
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.pixels_per_point = ctx.pixels_per_point();
        self.refresh_frame_now();
        self.apply_theme(ctx);
        let autoscroll_on = crate::autoscroll::enabled(self.engine.settings.middle_click_autoscroll);
        self.autoscroll.begin(ctx, autoscroll_on);
        if self.autoscroll.active() {
            self.scrolling.stop();
            ctx.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
        } else {
            self.scrolling.apply(ctx);
        }
        if self.engine.lyrics_fullscreen_restoring.is_some()
            && self.engine.lyrics_fullscreen_restoring == ctx.input(|input| input.viewport().fullscreen)
            && (!self.engine.lyrics_restore_maximized
                || ctx.input(|input| input.viewport().maximized == Some(true)))
        {
            self.engine.lyrics_fullscreen_restoring = None;
            self.engine.lyrics_restore_maximized = false;
        }
        if self.engine.lyrics_fullscreen.is_some() {
            if ctx.input(|input| input.viewport().fullscreen.unwrap_or(false)) {
                self.engine.lyrics_fullscreen_seen = true;
            } else if self.engine.lyrics_fullscreen_seen {
                self.leave_lyrics_fullscreen(ctx);
            }
        }
        // Switch to the main window when sign-in is required.
        let needs_sign_in = !(self.engine.is_connected() && self.engine.user.is_some())
            && !matches!(self.engine.auth, AuthStatus::Connecting | AuthStatus::Starting)
            && !(self.engine.is_connected() && self.engine.user.is_none());
        if self.engine.settings.winamp_window && needs_sign_in && !self.switch_intent {
            self.engine.actions.push(Action::ToggleWinampWindow);
        }
        if self.engine.settings.winamp_window {
            crate::ui::winamp::show(self, ui);
        } else {
            crate::ui::show(self, ui);
        }
        self.apply_actions(ctx);
        let autoscroll = self.autoscroll.finish(
            ctx,
            crate::autoscroll::enabled(self.engine.settings.middle_click_autoscroll),
        );
        if autoscroll.scrolling {
            self.scrolling.stop();
        }
        if autoscroll.stop_following_lyrics {
            self.engine.lyrics_following = false;
        }
        if let Some(offset) = autoscroll.playlist_scroll {
            self.winamp.playlist_scroll = offset;
        }
        self.refresh_frame_now();
        self.sync_media_controls(ctx);

        if !self.engine.toasts.is_empty() {
            ctx.request_repaint_after(TOAST_FRAME);
        }
        self.theme_transition.paint(ctx);
        self.engine.frame_now = None;
    }

    fn connected_repaint_interval(&self) -> Duration {
        let unseen = self.budget == crate::power::Budget::Background
            && !self.engine.now_playing().is_some_and(|now| now.playing);
        match self.engine.target() {
            Target::Local if self.engine.local.is_active() => REMOTE_POLL_IDLE,
            _ if unseen => REMOTE_POLL_UNSEEN,
            _ => REMOTE_POLL_ACTIVE,
        }
    }
}

impl App {
    fn refresh_frame_now(&mut self) {
        self.engine.frame_now = self.engine.requested_track_preview()
            .or_else(|| self.engine.now_playing_live())
            .or_else(|| self.engine.resume_preview());
    }

    pub fn small_art<'a>(&self, now: &'a NowPlaying, points: f32) -> Option<&'a str> {
        let small = now.art_small.as_deref();
        let large = now.art_url.as_deref();
        if self.art_px(points) > crate::api::models::SMALL_ART_WIDTH {
            large.or(small)
        } else {
            small.or(large)
        }
    }

    fn handle_events(&mut self) {
        let events = self.engine.backend.poll();
        self.handle_backend_events(events);
    }



    fn handle_local(&mut self, state: LocalState) {
        if self.engine.local_transfer_sequence
            .is_some_and(|sequence| sequence != state.track_sequence)
        {
            self.engine.local_transfer_sequence = None;
        }
        if state.track_sequence != self.engine.local.track_sequence
            && matches!(self.engine.target(), Target::Local)
        {
            self.engine.listening = None;
        }
        let track_changed =
            state.track != self.engine.local.track || state.track_sequence != self.engine.local.track_sequence;
        let reconnected = state.connected && !self.engine.local.connected;
        if state.shuffle != self.engine.local.shuffle
            && self.engine.shuffle_set_at
                .is_none_or(|at| at.elapsed() > Duration::from_secs(5))
        {
            // Accept shuffle changes made by another client.
            self.engine.shuffle_wanted = state.shuffle;
        }
        if state.playback != self.engine.local.playback {
            self.engine.optimistic_playing = None;
            if matches!(state.playback, Playback::Playing | Playback::Loading) {
                self.engine.clear_play_pending();
            }
        }
        if state.track != self.engine.local.track {
            self.engine.clear_play_pending();
        }
        self.engine.reconcile_local_track_intent(
            track_changed,
            state.track.as_ref().map(|track| track.uri.as_str()),
        );
        let held_volume = self.engine.held_local_volume(state.volume);
        if held_volume.is_none() && state.volume != self.engine.settings.volume {
            self.engine.settings.volume = state.volume;
            self.engine.settings_dirty = true;
        }
        if state.seek_sequence != self.engine.local.seek_sequence || track_changed {
            self.engine.pending_local_position = None;
        }
        if state.seek_sequence != self.engine.local.seek_sequence
            && let Some(controls) = &self.media_controls
        {
            controls.seeked(Duration::from_millis(u64::from(state.position_ms)));
        }
        if let Some(error) = &state.error
            && self.engine.local.error.as_deref() != Some(error.as_str())
        {
            self.engine.toast_error(engine_error_text(self.engine.localize.as_ref(), self.engine.locale, error));
            // One unavailable track is Spotify's catalogue; several in a
            // row is the session's audio-key service gone bad, which
            // leaves librespot feeding the decoder encrypted bytes and
            // skipping through the whole album. A fresh session cures it.
            if error.starts_with("This item isn't available") {
                let now = Instant::now();
                self.engine.unavailable_at
                    .retain(|at| now.duration_since(*at) < Duration::from_secs(20));
                self.engine.unavailable_at.push(now);
                if self.engine.unavailable_at.len() >= 3
                    && self.engine.last_unavailable_reconnect
                        .is_none_or(|at| at.elapsed() > Duration::from_secs(60))
                {
                    self.engine.unavailable_at.clear();
                    self.engine.last_unavailable_reconnect = Some(now);
                    self.engine.backend.send(Command::Reconnect);
                    self.engine.toast(self.engine.localize.t(
                        self.engine.locale,
                        "Spotify audio disconnected. Reconnecting local playback",
                    ));
                }
            }
        }
        if let Some(seed) = autoplay_seed(
            self.engine.local_list.as_deref(),
            self.engine.settings.autoplay,
            &self.engine.local,
            &state,
        ) {
            log::info!("the list ended; playing what Spotify follows {seed} with");
            self.engine.local_list = None;
            self.engine.backend.player(PlayerCommand::Load(LoadSpec {
                context_uri: Some(seed),
                play: true,
                autoplay: true,
                ..LoadSpec::default()
            }));
        }
        // Another device may have taken over while the last poll still
        // names this one.
        if self.engine.local.is_active() && !state.is_active() {
            self.engine.poll_remote_soon();
        }
        self.engine.local = state;
        if let Some(volume) = held_volume {
            self.engine.local.volume = volume;
        }
        if track_changed {
            self.on_now_playing_changed();
        }
        if reconnected {
            self.engine.replay_queued_play();
            // Retry names requested before the session connected.
            let unresolved: Vec<String> = self.engine.user_names
                .iter()
                .filter(|(_, name)| name.is_none())
                .map(|(id, _)| id.clone())
                .collect();
            for id in &unresolved {
                self.engine.user_names.remove(id);
            }
            self.engine.request_user_names(unresolved);
        }
    }



    fn on_now_playing_changed(&mut self) {
        // Queue consumption and saved session state follow confirmed playback,
        // not the song shown optimistically while a command is still pending.
        let Some(now) = self.engine.now_playing_live() else {
            return;
        };
        let same_uri = self.engine.last_now_playing_uri.as_deref() == Some(now.uri.as_str());
        let new_occurrence =
            now.local && self.engine.last_now_playing_sequence != self.engine.local.track_sequence;
        if same_uri && !new_occurrence {
            return;
        }
        // A new track is a new resume point.
        self.engine.session_dirty = true;
        let queue_already_updated =
            self.engine.queue_start_pending.take().as_ref() == Some(&self.engine.target());
        let repeating = same_uri && new_occurrence && now.repeat == RepeatMode::Track;
        // Restore the saved queue only when the remembered track resumes.
        if !self.engine.resume_queue.is_empty() {
            let queued = std::mem::take(&mut self.engine.resume_queue);
            self.engine.session_dirty = true;
            if now.local && self.engine.resume_track.as_deref() == Some(now.uri.as_str()) {
                for uri in queued {
                    self.engine.manual_queue.push(uri.clone());
                    self.engine.backend.api(ApiRequest::AddToQueue {
                        uri,
                        device_id: self.engine.local_device_id.clone(),
                        label: String::new(),
                    });
                }
            }
        }
        // A context load can start the same song as the manual queue's head
        // without consuming that queued copy. Explicit skips have already
        // consumed their rows; only an unrequested advance consumes one here.
        if !queue_already_updated && !repeating {
            self.engine.consume_manual_queue_head(&now.uri);
        }
        if !queue_already_updated
            && !repeating
            && let Loadable::Loaded(queue) = &mut self.engine.queue
        {
            let accounted = queue
                .currently_playing
                .as_ref()
                .is_some_and(|item| item.uri() == now.uri)
                && !(same_uri && new_occurrence);
            if !accounted
                && queue
                    .queue
                    .first()
                    .is_some_and(|item| item.uri() == now.uri)
            {
                let item = queue.queue.remove(0);
                queue.currently_playing = Some(item);
            }
        }
        self.engine.last_now_playing_uri = Some(now.uri.clone());
        if now.local {
            self.engine.last_now_playing_sequence = self.engine.local.track_sequence;
        }
        self.engine.resume_context = self.engine.playing_context_uri();
        self.engine.resume_track = Some(now.uri.clone());
        self.engine.resume_position_ms = 0;
        if now.local
            && !now.is_episode
            && let Some(id) = &now.id
            && !self.engine.track_cache.contains_key(id)
            && self.engine.track_requests.insert(id.clone())
        {
            self.engine.backend.api(ApiRequest::Track { id: id.clone() });
        }
        self.engine.request_contains(vec![now.uri.clone()]);
        if let Some(url) = now.art_small.or(now.art_url) {
            self.engine.tint_for(Some(&url));
        }
        if matches!(self.engine.page(), Page::Queue)
            || self.engine.show_queue_panel
            || (self.engine.settings.winamp_window && self.engine.settings.playlist_open)
        {
            self.engine.refresh_queue(true);
        }
        if self.engine.show_lyrics_panel {
            self.request_lyrics();
        }
    }

    pub fn request_lyrics(&mut self) {
        let Some(now) = self.engine.now_playing() else {
            return;
        };
        if self.engine.lyrics_uri.as_deref() == Some(now.uri.as_str())
            && !matches!(self.engine.lyrics, Loadable::NotLoaded | Loadable::Failed(_))
        {
            return;
        }
        self.engine.lyrics_uri = Some(now.uri.clone());
        self.engine.lyrics_following = true;
        self.engine.lyrics_line_shown = None;
        if now.is_episode || self.engine.offline {
            self.engine.lyrics = Loadable::Loaded(None);
            return;
        }
        self.engine.lyrics = Loadable::Loading;
        self.engine.backend.send(Command::Lyrics(Box::new(LyricsRequest {
            uri: now.uri,
            query: crate::lyrics::Query {
                artist: now
                    .artists
                    .first()
                    .map(|artist| artist.name.clone())
                    .unwrap_or_default(),
                title: now.title,
                album: now.album_name,
                duration_ms: now.duration_ms,
            },
        })));
    }

    fn request_proxy(&mut self, sign_in: bool) {
        let config = match self.engine.settings.proxy_config() {
            Ok(config) => config,
            Err(error) => {
                self.engine.toast_error(error);
                return;
            }
        };
        self.engine.proxy_request = self.engine.proxy_request.wrapping_add(1);
        let request = self.engine.proxy_request;
        self.pending_proxy_preferences
            .insert(request, self.engine.settings.proxy_preferences());
        self.engine.backend.send(if sign_in {
            Command::SignIn { request, config }
        } else {
            Command::ApplyProxy { request, config }
        });
    }

    fn handle_proxy_applied(
        &mut self,
        request: u64,
        config: spotifast_core::proxy::ProxyConfig,
        result: Result<bool, String>,
    ) {
        let Some(preferences) = self.pending_proxy_preferences.remove(&request) else {
            return;
        };
        if request < self.engine.last_proxy_applied {
            return;
        }
        match result {
            Ok(restarted) => {
                self.engine.last_proxy_applied = request;
                self.engine.applied_proxy = config;
                self.applied_proxy_preferences = preferences;
                self.save_settings();
                if restarted && self.engine.local_ready {
                    self.engine.toast(self.engine.localize.t(self.engine.locale, "Proxy applied. Restarting local playback."));
                } else {
                    self.engine.toast(self.engine.localize.t(self.engine.locale, "Proxy settings applied"));
                }
            }
            Err(error) => self.engine.toast_error(
                self.engine.localize.t(
                    self.engine.locale,
                    // Translators: {error} is an error message.
                    "Proxy could not be applied: {error}. Previous connection settings are still in use.",
                )
                .replace("{error}", &error.to_string()),
            ),
        }
    }



    fn custom_palette(&self) -> Option<Palette> {
        self.engine.settings.cached_palette()
    }

    pub fn save_state(&mut self) {
        self.save_settings();
        self.save_session();
    }

    fn handle_proxy_password_stored(&mut self) {
        if self.engine.settings.proxy_password_legacy {
            self.engine.settings.proxy_password_legacy = false;
            self.save_settings();
        }
    }

    fn poll_remote_if_due(&mut self) {
        let (interval, song_end) = self.remote_poll_interval();
        let elapsed = self.engine.remote_polled_at.elapsed();
        if self.engine.remote_poll_pending || elapsed < interval {
            return;
        }
        if song_end && elapsed < self.regular_poll_interval() {
            self.engine.remote_end_polled_at = Some(Instant::now());
        }
        self.engine.poll_remote(false);
    }

    fn remote_poll_interval(&self) -> (Duration, bool) {
        let interval = self.regular_poll_interval();
        match self.engine.remote_song_end() {
            Some(end) if end < interval => (end, true),
            _ => (interval, false),
        }
    }

pub fn milkdrop_command(&mut self, command: &str) {
        match command {
            "previous" => self.engine.actions.push(Action::Previous),
            "next" => self.engine.actions.push(Action::Next),
            "play-pause" => self.engine.actions.push(Action::TogglePlay),
            "mute" => self.engine.actions.push(Action::ToggleMute),
            "save-toggle" => {
                if let Some(now) = self.engine.now_playing().filter(|now| !now.is_episode) {
                    self.engine.actions.push(Action::ToggleSaved(now.uri));
                }
            }
            "shuffle" => self.engine.actions.push(Action::ToggleShuffle),
            "volume-up" => self.engine.actions.push(Action::VolumeBy(5)),
            "volume-down" => self.engine.actions.push(Action::VolumeBy(-5)),
            _ => {}
        }
    }

    pub fn sync_window_title(&mut self, ctx: &egui::Context) {
        let title = match self.engine.now_playing().filter(|now| now.playing) {
            Some(now) if now.subtitle.is_empty() => format!("{} - Spotifast", now.title),
            Some(now) => format!("{} - {}", now.subtitle, now.title),
            None => "Spotifast".to_string(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    pub fn open_folder(&mut self, folder: std::path::PathBuf) {
        let opened = std::fs::create_dir_all(&folder).and_then(|()| crate::opener::open(&folder));
        if let Err(error) = opened {
            self.engine.toast_error(
                // Translators: {folder} is a folder path and {error} is an error message.
                self.engine.localize.t(self.engine.locale, "Couldn't open {folder}: {error}")
                    .replace("{folder}", &folder.display().to_string())
                    .replace("{error}", &error.to_string()),
            );
        }
    }

pub fn thumb_state(&self, dark: bool) -> crate::thumbbar::ThumbState {
        let now = self.engine.now_playing();
        crate::thumbbar::ThumbState {
            has_track: now.is_some(),
            playing: now.as_ref().is_some_and(|now| now.playing),
            can_control: now.as_ref().is_some_and(|now| now.can_control),
            dark,
        }
    }

    pub fn background_activity(&self) -> Option<BackgroundActivity> {
        if self.engine.offline {
            return None;
        }
        let long =
            |since: Option<Instant>| since.is_some_and(|at| at.elapsed() >= BACKGROUND_QUIET);
        let [library, updates] = self.background_work_since();
        if long(library) {
            Some(BackgroundActivity::Library)
        } else if long(updates) {
            Some(BackgroundActivity::Updates)
        } else if self.engine.backend.activity().busy(BACKGROUND_QUIET) {
            Some(BackgroundActivity::Spotify)
        } else {
            None
        }
    }





    fn handle_api(&mut self, mut response: ApiResponse) {
        match &mut response {
            ApiResponse::Playlist {
                id,
                result: Ok(playlist),
                generation,
            } => {
                if self.engine.playlist_pages
                    .get(id)
                    .is_some_and(|page| page.generation == *generation)
                {
                    self.engine.reconcile_playlist_cover(id, &mut playlist.images);
                }
            }
            ApiResponse::MyPlaylists {
                result: Ok(page), ..
            } => {
                for playlist in &mut page.items {
                    self.engine.reconcile_playlist_cover(&playlist.id, &mut playlist.images);
                }
            }
            _ => {}
        }
        match response {
            ApiResponse::Me(result) => match result {
                Ok(user) => {
                    // Spotify only takes playback commands from Premium
                    // accounts, here or on any device, so a Free account
                    // is told once rather than left pressing play.
                    let free = user
                        .product
                        .as_deref()
                        .is_some_and(|product| product != "premium");
                    if free && !self.engine.premium_notice_shown {
                        self.engine.premium_notice_shown = true;
                        self.engine.dialog = Some(Dialog::PremiumNeeded);
                    }
                    let due = std::mem::take(&mut self.engine.account_caches_due);
                    let changed = self.engine.user_id() != Some(user.id.as_str());
                    if changed {
                        self.engine.rootlist = self.engine.rootlist_cache
                            .as_ref()
                            .filter(|cached| cached.account_id == user.id)
                            .map(|cached| cached.entries.clone())
                            .unwrap_or_default();
                        self.engine.editable_by_grant.clear();
                    }
                    if changed || due {
                        self.engine.request_account_caches();
                    }
                    self.engine.user = Some(user);
                    let page = self.engine.page().clone();
                    self.engine.ensure_loaded(page);
                    if let Some(now) = self.engine.now_playing() {
                        self.engine.request_contains(vec![now.uri]);
                    }
                }
                Err(error) => {
                    if matches!(error, crate::api::ApiError::SignInExpired { .. }) {
                        self.engine.auth = AuthStatus::Failed(
                            self.engine.localize.t(
                                self.engine.locale,
                                "Your Spotify sign-in expired. Please sign in again.",
                            )
                            .into_owned(),
                        );
                    } else {
                        self.engine.toast_error(
                            // Translators: {error} is an error message.
                            self.engine.localize.t(self.engine.locale, "Couldn't load your profile: {error}")
                                .replace("{error}", &error.to_string()),
                        );
                    }
                }
            },
            ApiResponse::Devices(result) => {
                self.engine.devices_loading = false;
                self.engine.devices_fetched_at = Some(Instant::now());
                match result {
                    Ok(devices) => {
                        self.engine.devices = devices;
                        self.engine.control_devices_stale = true;
                        if let Some((name, since)) = self.engine.pending_transfer_to.clone() {
                            // The receiver's own id keeps another device of the
                            // same name from taking over. A rescan can drop the
                            // receiver; then the name has to do.
                            let ids: Vec<_> = self.engine.receivers
                                .iter()
                                .filter(|receiver| receiver.name == name)
                                .filter_map(|receiver| receiver.device_id.clone())
                                .collect();
                            let matching = self.engine.devices
                                .iter()
                                .find(|device| {
                                    device.name == name
                                        && (ids.is_empty()
                                            || device
                                                .id
                                                .as_ref()
                                                .is_some_and(|id| ids.contains(id)))
                                })
                                .and_then(|device| device.id.clone());
                            if let Some(id) = matching {
                                self.engine.pending_transfer_to = None;
                                let play = self.engine.activating_play
                                    || self.engine.now_playing().is_some_and(|now| now.playing);
                                self.engine.transfer_remote(id, play);
                            } else if since.elapsed() > Duration::from_secs(20) {
                                self.engine.pending_transfer_to = None;
                            } else {
                                self.engine.devices_fetched_at = None;
                            }
                        }
                        if let Some(selected) = &self.engine.selected_device
                            && !self.engine.receivers.iter().any(|receiver| {
                                crate::sonos::is_sonos(receiver)
                                    && receiver.device_id.as_deref() == Some(selected.as_str())
                            })
                            && !self.engine.devices
                                .iter()
                                .any(|device| device.id.as_deref() == Some(selected.as_str()))
                        {
                            self.engine.selected_device = None;
                        }
                    }
                    Err(error) => self.engine.toast_error(
                        // Translators: {error} is an error message.
                        self.engine.localize.t(self.engine.locale, "Couldn't list devices: {error}")
                            .replace("{error}", &error.to_string()),
                    ),
                }
            }
            ApiResponse::PlaybackState { seq, result } => {
                if seq != self.engine.remote_poll_seq {
                    // An older poll finishing late describes the past.
                    return;
                }
                self.engine.remote_poll_pending = false;
                match result {
                    Ok(state) => {
                        let previous_uri = self.engine.remote.as_ref().and_then(|remote| {
                            remote
                                .state
                                .item
                                .as_ref()
                                .map(|item| item.uri().to_string())
                        });
                        let previous_shuffle = self.engine.remote
                            .as_ref()
                            .map(|remote| remote.state.shuffle_state);
                        self.engine.remote = state.map(|state| RemoteSnapshot {
                            state,
                            received_at: Instant::now(),
                        });
                        if let (Some(previous), Some(current)) = (
                            previous_shuffle,
                            self.engine.remote
                                .as_ref()
                                .map(|remote| remote.state.shuffle_state),
                        ) && previous != current
                            && self.engine.shuffle_set_at
                                .is_none_or(|at| at.elapsed() > Duration::from_secs(5))
                        {
                            // Accept shuffle changes from another device.
                            self.engine.shuffle_wanted = current;
                        }
                        if let Some(context) = self.engine.remote
                            .as_ref()
                            .and_then(|remote| remote.state.context.as_ref())
                            .map(|context| context.uri.clone())
                        {
                            // Ignore the old context while a takeover settles.
                            let stale = self.engine.assumed_context.as_ref().is_some_and(|assumed| {
                                assumed.at.elapsed() < ASSUMED_CONTEXT_HOLD
                                    && assumed.uri != context
                            });
                            if !stale {
                                self.engine.note_recent_context(&context);
                            }
                        }
                        let uri = self.engine.remote.as_ref().and_then(|remote| {
                            remote
                                .state
                                .item
                                .as_ref()
                                .map(|item| item.uri().to_string())
                        });
                        self.engine.reconcile_remote_track_intent(seq, uri.as_deref());
                        if let Some(remote) = &self.engine.remote
                            && let Some(device) = &remote.state.device
                            && device.id.is_some()
                            && let Some(known) =
                                self.engine.devices.iter_mut().find(|known| known.id == device.id)
                        {
                            known.is_active = true;
                            known.volume_percent = device.volume_percent;
                        }
                        if let Some((wanted, _)) = self.engine.optimistic_playing
                            && self.engine.remote
                                .as_ref()
                                .is_some_and(|remote| remote.state.is_playing == wanted)
                        {
                            self.engine.optimistic_playing = None;
                        }
                        if uri != previous_uri {
                            self.on_now_playing_changed();
                        }
                    }
                    Err(error) => log::debug!("playback state unavailable: {error}"),
                }
            }
            ApiResponse::Queue { seq, result } => {
                if seq != self.engine.queue_seq {
                    // A newer request supersedes this response.
                    return;
                }
                // Partial reads and read failures must preserve optimistic
                // additions while Spotify is still accepting their writes.
                let writing_queue = self.engine.pending_queue_batches
                    .values()
                    .any(|target| *target == self.engine.target());
                if writing_queue
                    || result
                        .as_ref()
                        .is_ok_and(|fetched| self.engine.queue_fetch_is_stale(fetched))
                {
                    // Keep the optimistic queue and retry after a stale
                    // response. Accept Spotify's state after the retry limit.
                    self.engine.queue_stale_retries = self.engine.queue_stale_retries.saturating_add(1);
                    self.engine.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
                    return;
                }
                self.engine.queue_stale_retries = 0;
                if result.is_ok() {
                    self.engine.queue_cleared = None;
                    self.engine.queue_shuffle_pending = None;
                    self.engine.queue_reorder_pending = None;
                    self.engine.queue_removed = None;
                }
                self.engine.queue = Loadable::from_result(result);
                self.engine.reconcile_pending_queue();
                if let Some(queue) = self.engine.queue.get() {
                    let uris: Vec<String> = queue
                        .queue
                        .iter()
                        .map(|item| item.uri().to_string())
                        .collect();
                    self.engine.request_contains(uris);
                }
            }
            ApiResponse::RecentlyPlayed {
                who,
                generation,
                limit,
                result,
            } => match who {
                RecentsFor::Home => {
                    if generation != self.engine.home.generation {
                        return;
                    }
                    if let Ok(page) = &result {
                        self.engine.note_recent_contexts(&page.items);
                    }
                    let items = result
                        .as_ref()
                        .map(|page| page.items.clone())
                        .map_err(|error| error.to_string());
                    let changed = items.is_ok();
                    self.engine.home.refresh_failed |= !changed;
                    self.engine.home.recently_played.refresh(items);
                    self.engine.home_answered(changed);
                }
                RecentsFor::Panel => {
                    if generation != self.engine.recents_generation {
                        return;
                    }
                    self.engine.recents.loading = false;
                    self.engine.recents.loaded_once = true;
                    match result {
                        Ok(page) => self.engine.absorb_recents(page, limit),
                        Err(error) => self.engine.recents.error = Some(error.to_string()),
                    }
                }
            },
            ApiResponse::TopTracks {
                offset,
                full,
                generation,
                result,
            } => {
                if full {
                    if generation != self.engine.home.top_songs_generation {
                        return;
                    }
                    match result {
                        Ok(page) => {
                            let received = page.items.len() as u32;
                            let tracks = page.items;
                            let uris: Vec<String> =
                                tracks.iter().map(|track| track.uri.clone()).collect();
                            self.engine.request_contains(uris);
                            if offset == 0 {
                                self.engine.home.top_songs = Loadable::Loaded(tracks);
                            } else if let Some(current) = self.engine.home.top_songs.get_mut() {
                                current.extend(tracks);
                            }
                            if page.next.is_some() && received > 0 && offset + received < 100 {
                                self.engine.backend.api(ApiRequest::TopTracks {
                                    offset: offset + received,
                                    full: true,
                                    generation,
                                });
                            } else {
                                self.engine.home.top_songs_loading = false;
                                self.engine.home.top_songs_complete = true;
                            }
                        }
                        Err(error) => {
                            self.engine.home.top_songs.refresh(Err::<Vec<Track>, _>(error));
                            self.engine.home.top_songs_loading = false;
                        }
                    }
                } else if generation != self.engine.home.generation {
                } else if let Ok(page) = result {
                    let tracks = page.items;
                    let seeds: Vec<String> = tracks
                        .iter()
                        .filter_map(|track| track.id.clone())
                        .take(5)
                        .collect();
                    if !seeds.is_empty() {
                        if self.engine.home.recommendations.get().is_none() {
                            self.engine.home.recommendations = Loadable::Loading;
                        }
                        self.engine.backend.api(ApiRequest::Recommendations {
                            seed_tracks: seeds,
                            seed_artists: Vec::new(),
                            generation,
                        });
                    }
                    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();
                    self.engine.request_contains(uris);
                    self.engine.home.top_tracks = Loadable::Loaded(tracks);
                    self.engine.home_answered(true);
                } else if let Err(error) = result {
                    if offset == 0 && self.engine.home.top_tracks.get().is_none() {
                        self.engine.home.top_tracks = Loadable::Failed(error.to_string());
                    }
                    self.engine.home.refresh_failed = true;
                    self.engine.home_answered(false);
                }
            }
            ApiResponse::TopArtists { generation, result } => {
                if generation != self.engine.home.generation {
                    return;
                }
                let changed = result.is_ok();
                self.engine.home.refresh_failed |= !changed;
                self.engine.home.top_artists.refresh(result);
                self.engine.home_answered(changed);
            }
            ApiResponse::Recommendations { generation, result } => {
                if generation != self.engine.home.generation {
                    return;
                }
                if let Ok(tracks) = &result {
                    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();
                    self.engine.request_contains(uris);
                }
                self.engine.home.recommendations.refresh(result);
            }
            ApiResponse::MadeForYou {
                generation,
                personalized,
                result,
            } => {
                if generation != self.engine.home.made_for_you_generation {
                    return;
                }
                let changed = result.is_ok();
                self.engine.home.refresh_failed |= !changed;
                if changed {
                    self.engine.home.made_for_you_personalized = personalized;
                }
                self.engine.home.made_for_you.refresh(result);
                if self.engine.home.made_for_you_loading {
                    self.engine.home.made_for_you_loading = false;
                    self.engine.home_answered(changed);
                }
            }
            // A reload reads the playlists from the top again under a new
            // generation, so a page any earlier load asked for no longer
            // continues the list, whatever its offset. Within one load, only
            // the later page on its way is taken, and only once.
            ApiResponse::MyPlaylists {
                offset, generation, ..
            } if generation != self.engine.library.playlists_generation
                || (offset > 0 && self.engine.library.playlists_asked != Some(offset)) => {}
            ApiResponse::MyPlaylists { offset, result, .. } => match result {
                Ok(page) => {
                    self.engine.library.playlists_asked = None;
                    let next_offset = page.next_offset();
                    let shown = match &mut self.engine.library.playlists_incoming {
                        Some(incoming) => {
                            incoming.extend(page.items);
                            false
                        }
                        None => {
                            match &mut self.engine.library.playlists {
                                Loadable::Loaded(existing) if offset > 0 => {
                                    existing.extend(page.items)
                                }
                                slot => *slot = Loadable::Loaded(page.items),
                            }
                            true
                        }
                    };
                    self.engine.library.playlists_next = next_offset;
                    if next_offset.is_some() {
                        self.engine.load_more(Page::Home);
                        if self.engine.library.playlists_asked.is_none() {
                            // Offline, the next page was not asked for. The
                            // load ends with what is shown, and the network
                            // coming back reads the list again.
                            let library = &mut self.engine.library;
                            library.playlists_incoming = None;
                            library.playlists_loading_since = None;
                            library.playlists_retry_at = Some(Instant::now());
                        }
                    } else {
                        let generation = self.engine.library.playlists_generation;
                        let library = &mut self.engine.library;
                        if let Some(incoming) = library.playlists_incoming.take() {
                            library.playlists = Loadable::Loaded(incoming);
                        }
                        library.playlists_whole = true;
                        library.playlists_loading_since = None;
                        // This load was asked for after these changes were
                        // confirmed, so its list tells of them itself.
                        library
                            .playlists_edits
                            .retain(|edit| edit.settled_by.is_none_or(|by| by > generation));
                        // Load folder order after all playlists arrive.
                        self.engine.read_rootlist();
                    }
                    if shown || next_offset.is_none() {
                        self.engine.apply_playlist_edits();
                        self.engine.fill_blank_playlists();
                        self.engine.note_listed_playlists();
                    }
                    if next_offset.is_none() {
                        self.engine.store_library_cache();
                    }
                }
                Err(error) => {
                    let library = &mut self.engine.library;
                    library.playlists_asked = None;
                    library.playlists_loading_since = None;
                    let refreshing = library.playlists_incoming.take().is_some();
                    if matches!(
                        error,
                        crate::api::ApiError::RateLimited | crate::api::ApiError::Network(_)
                    ) || error.status().is_some_and(|status| status >= 500)
                    {
                        library.playlists_retry_at = Some(Instant::now() + PLAYLISTS_RETRY);
                    }
                    if refreshing {
                        // The list shown stays; it is read again later.
                        log::warn!("could not refresh the playlist list: {error}");
                    } else if offset == 0 {
                        library.playlists = Loadable::Failed(error.to_string());
                    } else {
                        self.engine.toast_error(
                            // Translators: {error} is an error message.
                            self.engine.localize.t(self.engine.locale, "Couldn't load more playlists: {error}")
                                .replace("{error}", &error.to_string()),
                        );
                    }
                }
            },
            ApiResponse::Playlist {
                id,
                generation,
                mut result,
            } => {
                if self.engine.playlist_pages
                    .get(&id)
                    .is_none_or(|page| page.generation != generation || page.pending_writes > 0)
                {
                    return;
                }
                let stale_write_snapshot = self.engine.playlist_pages.get_mut(&id).is_some_and(|page| {
                    let Some(expected) = page.optimistic_snapshot.as_deref() else {
                        return false;
                    };
                    let reported = result
                        .as_ref()
                        .ok()
                        .and_then(|playlist| playlist.snapshot_id.as_deref());
                    if reported == Some(expected) {
                        page.optimistic_snapshot = None;
                        page.snapshot_rechecks = 0;
                        false
                    } else if result.is_ok() {
                        page.snapshot_rechecks = page.snapshot_rechecks.saturating_add(1);
                        true
                    } else {
                        false
                    }
                });
                if stale_write_snapshot {
                    let page = self.engine.playlist_pages.get_mut(&id).unwrap();
                    if page.snapshot_rechecks <= 3 {
                        self.engine.backend.api(ApiRequest::Playlist { id, generation });
                    } else {
                        page.refresh_after_write = false;
                        page.items.fail(
                            self.engine.localize.t(
                                self.engine.locale,
                                "Spotify hasn't confirmed your playlist changes yet. Try refreshing again.",
                            )
                            .into_owned(),
                        );
                    }
                    return;
                }
                if let Ok(playlist) = &mut result {
                    // A header read over the streaming session lacks what
                    // the Web API gave: the account's own name, and the
                    // library list's public flag, owner name, and cover.
                    if playlist.owner.display_name.is_none() {
                        playlist.owner.display_name = self.engine.own_name(playlist.owner.id.as_deref());
                    }
                    // The owner's avatar comes from the profile view only
                    // the session can ask, so whoever owns the page shown
                    // is asked about once, next to the names already being
                    // resolved for the songs' adders.
                    if let Some(owner) = playlist.owner.id.clone().filter(|owner| !owner.is_empty())
                    {
                        self.engine.request_user_names(vec![owner]);
                    }
                    if let Some(listed) = self.engine.library_entry(&id) {
                        playlist.fill_from(listed);
                    }
                    if let Some(image) = pick_image(&playlist.images, 64) {
                        self.engine.tint_for(Some(image));
                    }
                }
                let mut refresh_rows = false;
                if let Some(page) = self.engine.playlist_pages.get_mut(&id) {
                    if page.refresh_after_write || page.optimistic_snapshot.is_some() {
                        match &result {
                            Ok(_) => refresh_rows = page.refresh_after_write,
                            Err(error) => {
                                page.refresh_after_write = false;
                                page.items.fail(friendly_page_error(self.engine.localize.as_ref(), self.engine.locale, error));
                            }
                        }
                    }
                    let old_snapshot = page
                        .playlist
                        .get()
                        .and_then(|playlist| playlist.snapshot_id.as_deref());
                    let new_snapshot = result
                        .as_ref()
                        .ok()
                        .and_then(|playlist| playlist.snapshot_id.as_deref());
                    if old_snapshot.is_some() && old_snapshot != new_snapshot {
                        page.items.clear_windows();
                        page.cache_saved_through = None;
                        page.cache_saved_rows = 0;
                        page.cache_saved_total = None;
                        page.cache_append_valid = false;
                        page.cache_restored_through = None;
                    }
                    if let Ok(playlist) = &result
                        && page.items.loaded_once
                    {
                        page.items.total = Some(playlist.track_total());
                    }
                    page.playlist.refresh(result);
                }
                self.engine.try_adopt_playlist_cache(&id);
                self.engine.checkpoint_playlist_cache(&id);
                if refresh_rows {
                    self.engine.reload(Page::Playlist(id));
                }
            }
            ApiResponse::PlaylistItems {
                id,
                offset,
                generation,
                result,
            } => {
                if self.engine.playlist_pages.get(&id).is_none_or(|page| {
                    page.generation != generation
                        || page.pending_writes > 0
                        || page.optimistic_snapshot.is_some()
                }) {
                    return;
                }
                let mut uris = Vec::new();
                let mut adders: Vec<String> = Vec::new();
                let mut tracks = Vec::new();
                let cache_key = Page::Playlist(id.clone());
                if let Some(page) = self.engine.playlist_pages.get_mut(&id) {
                    match result {
                        _ if page
                            .cache_restored_through
                            .is_some_and(|cached| offset < cached)
                            && page.items.window_request != Some(offset) =>
                        {
                            // The initial request was already in flight when
                            // a longer cached prefix was restored.
                        }
                        Ok(mut items) => {
                            note_availability(&mut self.engine.playlist_availability, &items.items);
                            fill_availability(&self.engine.playlist_availability, &mut items.items);
                            tracks = items
                                .items
                                .iter()
                                .filter_map(|item| match item.playable() {
                                    Some(PlayableItem::Track(track)) => Some(track.clone()),
                                    _ => None,
                                })
                                .collect();
                            uris = items
                                .items
                                .iter()
                                .filter_map(|item| item.playable())
                                .map(|item| item.uri().to_string())
                                .collect();
                            adders = items
                                .items
                                .iter()
                                .filter_map(|item| item.added_by.as_ref()?.id.clone())
                                .filter(|id| !id.is_empty())
                                .collect();
                            page.contributors.extend(adders.iter().cloned());
                            let extends_checkpoint = offset > 0
                                && page.items.base_offset == 0
                                && page.items.loaded_once
                                && page.items.window_request.is_none()
                                && page.items.windows.is_empty()
                                && page.items.next_offset == Some(offset)
                                && page.items.total == Some(items.total)
                                && page.items.items.len() <= offset as usize
                                && page.items_generation == generation;
                            let extends_cached_rows = offset > 0
                                && page.items.loaded_once
                                && page.items.window_request.is_none()
                                && page.items.windows.is_empty()
                                && page.items.next_offset == Some(offset)
                                && page.items.total == Some(items.total)
                                && u32::try_from(page.items.items.len())
                                    .ok()
                                    .and_then(|len| page.items.base_offset.checked_add(len))
                                    == Some(offset)
                                && self.engine.table_rows.get(&cache_key).is_some_and(|cache| {
                                    cache.generation == generation
                                        && cache.items_revision == page.items.revision
                                        && cache.user_names_revision == self.engine.user_names_revision
                                        && cache.playlist_positions.is_some()
                                        && cache.playlist_raw_count == page.items.items.len()
                                });
                            if !extends_checkpoint {
                                page.cache_append_valid = false;
                            }
                            page.items.absorb(offset, items);
                            page.items_generation = generation;
                            if extends_cached_rows
                                && let Some(cache) = self.engine.table_rows.get_mut(&cache_key)
                            {
                                cache.playlist_append_revision = Some(page.items.revision);
                            }
                        }
                        Err(error) => page.items.fail(friendly_page_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                    }
                }
                for track in &tracks {
                    self.engine.remember_track_recording(track);
                }
                self.engine.request_contains(uris);
                self.engine.request_user_names(adders);
                self.engine.sample_playlist_tail(&id);
                self.engine.checkpoint_playlist_cache(&id);
                // A sorted table means the whole list, not the loaded part.
                if self.engine.table_sorts.contains_key(&Page::Playlist(id.clone())) {
                    self.engine.load_more(Page::Playlist(id));
                }
            }
            ApiResponse::PlaylistSample {
                id,
                generation,
                result,
            } => {
                if self.engine.playlist_pages
                    .get(&id)
                    .is_none_or(|page| page.generation != generation)
                {
                    return;
                }
                let mut adders: Vec<String> = Vec::new();
                if let Ok(items) = result
                    && let Some(page) = self.engine.playlist_pages.get_mut(&id)
                {
                    adders = items
                        .items
                        .iter()
                        .filter_map(|item| item.added_by.as_ref()?.id.clone())
                        .filter(|id| !id.is_empty())
                        .collect();
                    page.contributors.extend(adders.iter().cloned());
                }
                self.engine.request_user_names(adders);
            }
            ApiResponse::PlaylistCreated(result) => {
                self.engine.playlist_busy = false;
                match result {
                    Ok(playlist) => {
                        self.engine.toast(
                            // Translators: {name} is a playlist name.
                            self.engine.localize.t(self.engine.locale, "Created {name}")
                                .replace("{name}", &playlist.name),
                        );
                        let settled_by = self.engine.library.playlists_generation + 1;
                        self.engine.edit_playlists(PlaylistEdit {
                            uri: playlist.uri.clone(),
                            added: Some(playlist.clone()),
                            settled_by: Some(settled_by),
                        });
                        self.engine.saved.insert(playlist.uri.clone(), true);
                        self.engine.store_library_cache();
                        if let Some(Dialog::CreatePlaylist { add_uris, .. }) = self.engine.dialog.take()
                            && !add_uris.is_empty()
                        {
                            self.engine.backend.api(ApiRequest::AddToPlaylist {
                                playlist_id: playlist.id.clone(),
                                playlist_name: playlist.name.clone(),
                                uris: add_uris,
                                position: None,
                            });
                        }
                        self.engine.open(Page::Playlist(playlist.id));
                    }
                    Err(error) => {
                        self.engine.toast_error(
                            // Translators: {error} is an error message.
                            self.engine.localize.t(self.engine.locale, "Couldn't create the playlist: {error}")
                                .replace("{error}", &error.to_string()),
                        )
                    }
                }
            }
            ApiResponse::PlaylistCoverUploaded {
                id,
                request,
                previous_urls,
                cover,
                result,
            } => {
                if self.engine.cover_uploads.get(&id) != Some(&request) {
                    return;
                }
                self.engine.cover_uploads.remove(&id);
                if let Some(Dialog::EditPlaylist {
                    id: current,
                    cover: draft,
                    ..
                }) = &mut self.engine.dialog
                    && *current == id
                    && draft.uploading == Some(request)
                {
                    draft.uploading = None;
                    draft.error = result
                        .as_ref()
                        .err()
                        .map(|error| cover_error(self.engine.localize.as_ref(), self.engine.locale, error));
                    if result.is_ok() {
                        draft.selection = None;
                    }
                }
                match result {
                    Ok(()) => {
                        self.engine.set_playlist_cover(&id, &cover);
                        self.engine.uploaded_covers.insert(
                            id.clone(),
                            crate::playlist_cover::PendingCover {
                                cover,
                                previous_urls,
                                request,
                                checking: None,
                                next_images: None,
                                rechecks_left: 3,
                            },
                        );
                        if let Some(page) = self.engine.playlist_pages.get(&id) {
                            self.engine.backend.api(ApiRequest::Playlist {
                                id,
                                generation: page.generation,
                            });
                        }
                        self.engine.toast(self.engine.localize.t(self.engine.locale, "Playlist cover updated"));
                    }
                    Err(error) => self.engine.toast_error(cover_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                }
            }
            ApiResponse::PlaylistUpdated { id, result } => {
                self.engine.playlist_busy = false;
                match result {
                    Ok(()) => {
                        self.engine.toast(self.engine.localize.t(self.engine.locale, "Playlist updated"));
                        self.engine.playlist_pages.remove(&id);
                        self.engine.load_playlists();
                        if matches!(self.engine.page(), Page::Playlist(current) if *current == id) {
                            self.engine.ensure_loaded(Page::Playlist(id));
                        }
                    }
                    Err(error) => {
                        self.engine.toast_error(
                            // Translators: {error} is an error message.
                            self.engine.localize.t(self.engine.locale, "Couldn't update the playlist: {error}")
                                .replace("{error}", &error.to_string()),
                        )
                    }
                }
            }
            ApiResponse::PlaylistDuplicatesChecked {
                playlist_id,
                playlist_name,
                items,
                position,
                result,
            } => match result {
                Ok(duplicate_uris) if duplicate_uris.is_empty() => {
                    self.engine.add_to_playlist_now(playlist_id, playlist_name, items, position)
                }
                Ok(duplicate_uris) => {
                    self.engine.playlist_busy = false;
                    self.engine.dialog = Some(Dialog::ConfirmPlaylistDuplicates {
                        playlist_id,
                        playlist_name,
                        items,
                        position,
                        duplicate_uris,
                    });
                }
                Err(error) => {
                    // A failed read must not take away an edit the account is
                    // still allowed to make. The write reports its own error.
                    log::debug!("could not check playlist for duplicates: {error}");
                    self.engine.add_to_playlist_now(playlist_id, playlist_name, items, position);
                }
            },
            ApiResponse::PlaylistItemsChanged {
                id,
                message,
                result,
            } => {
                self.engine.playlist_busy = false;
                if let Some(page) = self.engine.playlist_pages.get_mut(&id) {
                    page.pending_writes = page.pending_writes.saturating_sub(1);
                }
                match result {
                    Ok(snapshot) => {
                        if !message.is_empty() {
                            self.engine.toast(message);
                        }
                        let mut generation = None;
                        if let Some(page) = self.engine.playlist_pages.get_mut(&id) {
                            if let Some(playlist) = page.playlist.get_mut() {
                                playlist.snapshot_id = snapshot.clone();
                            }
                            if snapshot.is_some() {
                                page.optimistic_snapshot = snapshot.clone();
                                page.snapshot_rechecks = 0;
                            }
                            generation = Some(page.generation);
                        }
                        if let Some(playlists) = self.engine.library.playlists.get_mut() {
                            for playlist in playlists.iter_mut().filter(|p| p.id == id) {
                                playlist.snapshot_id = snapshot.clone();
                            }
                        }
                        self.engine.checkpoint_playlist_cache(&id);
                        // Reconcile metadata in the background. The mutation
                        // response and local rows are enough to keep the page;
                        // no item page is discarded or downloaded again.
                        if let Some(generation) = generation {
                            self.engine.backend.api(ApiRequest::Playlist { id, generation });
                        }
                    }
                    Err(error) => {
                        self.engine.toast_error(
                            // Translators: {error} is an error message.
                            self.engine.localize.t(self.engine.locale, "Playlist change failed: {error}")
                                .replace("{error}", &error.to_string()),
                        );
                        if let Some(page) = self.engine.playlist_pages.get_mut(&id) {
                            page.optimistic_snapshot = None;
                            page.snapshot_rechecks = 0;
                        }
                        self.engine.load_playlist_items_at(&id, 0);
                        if let Some(page) = self.engine.playlist_pages.get(&id) {
                            self.engine.backend.api(ApiRequest::Playlist {
                                id,
                                generation: page.generation,
                            });
                        }
                    }
                }
            }
            ApiResponse::PlaylistFollowChanged {
                id,
                followed,
                result,
            } => match result {
                Ok(()) => {
                    self.engine.settle_playlist_edit(&format!("spotify:playlist:{id}"), true);
                    self.engine.saved
                        .insert(format!("spotify:playlist:{id}"), followed);
                    self.engine.toast(if followed {
                        self.engine.localize.t(self.engine.locale, "Added to Your Library")
                    } else {
                        self.engine.localize.t(self.engine.locale, "Removed from Your Library")
                    });
                    self.engine.load_playlists();
                    if !followed && matches!(self.engine.page(), Page::Playlist(current) if *current == id)
                    {
                        self.engine.open(Page::Home);
                    }
                }
                Err(error) => {
                    self.engine.settle_playlist_edit(&format!("spotify:playlist:{id}"), false);
                    self.engine.saved
                        .insert(format!("spotify:playlist:{id}"), !followed);
                    self.engine.toast_error(
                        // Translators: {error} is an error message.
                        self.engine.localize.t(self.engine.locale, "Couldn't update the playlist: {error}")
                            .replace("{error}", &error.to_string()),
                    );
                }
            },
            ApiResponse::SavedTracks {
                offset,
                generation,
                account_id,
                result,
            } => {
                if generation != self.engine.liked_songs.generation
                    || self.engine.user_id() != account_id.as_deref()
                {
                    return;
                }
                let mut more = false;
                let succeeded = result.is_ok();
                match result {
                    Ok(page) => {
                        for item in &page.items {
                            let uri = item.track.uri.clone();
                            self.engine.remember_track_recording(&item.track);
                            if !self.engine.saved_writes.contains_key(&uri)
                                && self.engine.liked_songs.intent(&uri).is_none()
                            {
                                self.engine.set_saved_state(uri, true);
                            }
                        }
                        more = self.engine.liked_songs.absorb(
                            offset,
                            page,
                            jiff::Timestamp::now().as_second(),
                        );
                    }
                    Err(error) => self.engine.liked_songs
                        .fail(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                }
                if !more {
                    self.engine.sync_liked_songs();
                    self.engine.checkpoint_liked_songs(false);
                }
                if succeeded && (more || self.engine.table_sorts.contains_key(&Page::LikedSongs)) {
                    self.engine.load_more(Page::LikedSongs);
                }
            }
            // A reset shelf is read again from the top, so a page asked for
            // before the reset no longer continues it.
            ApiResponse::SavedAlbums { offset, .. }
                if self.engine.library.albums.next_offset != Some(offset) => {}
            ApiResponse::SavedAlbums { offset, result } => match result {
                Ok(page) => {
                    self.engine.request_album_types(page.items.iter().map(|item| &item.album));
                    for item in &page.items {
                        self.engine.saved.insert(item.album.uri.clone(), true);
                    }
                    self.engine.library.albums.absorb(offset, page);
                }
                Err(error) => self.engine.library
                    .albums
                    .fail(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
            },
            ApiResponse::FollowedArtists { after, result } => {
                let list = &mut self.engine.library.artists;
                // A reset shelf is read again from the top, so a page asked
                // for before the reset no longer continues it.
                if after != list.after {
                    return;
                }
                list.loading = false;
                list.loaded_once = true;
                match result {
                    Ok(page) => {
                        if after.is_none() {
                            list.items.clear();
                        }
                        let received = page.items.len();
                        for artist in &page.items {
                            self.engine.saved.insert(artist.uri.clone(), true);
                        }
                        list.items.extend(page.items);
                        let next = page.cursors.and_then(|cursors| cursors.after);
                        list.complete = next.is_none() || received == 0;
                        list.after = next;
                        list.error = None;
                    }
                    Err(error) => list.error = Some(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                }
            }
            ApiResponse::SavedShows { offset, .. }
                if self.engine.library.shows.next_offset != Some(offset) => {}
            ApiResponse::SavedShows { offset, result } => match result {
                Ok(page) => {
                    for item in &page.items {
                        self.engine.saved.insert(item.show.uri.clone(), true);
                    }
                    let unknown: Vec<String> = page
                        .items
                        .iter()
                        .map(|item| item.show.uri.clone())
                        .filter(|uri| self.engine.audiobooks_requested.insert(uri.clone()))
                        .collect();
                    if !unknown.is_empty() {
                        self.engine.backend.send(Command::AudiobookShows(unknown));
                    }
                    self.engine.library.shows.absorb(offset, page);
                    if offset == 0 && self.engine.home.requested {
                        self.engine.request_home_episodes();
                    }
                }
                Err(error) => self.engine.library
                    .shows
                    .fail(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
            },
            ApiResponse::HomeEpisodes { generation, .. } if generation != self.engine.home.generation => {}
            ApiResponse::HomeEpisodes { result, .. } => match result {
                Ok(podcasts) => {
                    self.engine.home.podcasts = podcasts;
                    self.engine.home.podcasts_answered = true;
                    self.engine.store_home_cache();
                }
                // The shelf is an extra: without an answer it keeps what it
                // showed, or stays hidden.
                Err(error) => log::debug!("podcast episodes for Home unavailable: {error}"),
            },
            ApiResponse::SavedEpisodes { offset, .. }
                if self.engine.library.episodes.next_offset != Some(offset) => {}
            ApiResponse::SavedEpisodes { offset, result } => match result {
                Ok(page) => {
                    for item in &page.items {
                        self.engine.saved.insert(item.episode.uri.clone(), true);
                    }
                    self.engine.library.episodes.absorb(offset, page);
                }
                Err(error) => self.engine.library
                    .episodes
                    .fail(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
            },
            ApiResponse::SavedChanged {
                uris,
                saved,
                result,
            } => {
                let current_uris: Vec<String> = uris
                    .iter()
                    .filter(|uri| {
                        self.engine.saved_writes
                            .get(*uri)
                            .is_none_or(|wanted| *wanted == saved)
                    })
                    .cloned()
                    .collect();
                for uri in &uris {
                    if self.engine.saved_writes.get(uri) == Some(&saved) {
                        self.engine.saved_writes.remove(uri);
                    }
                }
                match result {
                    Ok(()) => {
                        for uri in &current_uris {
                            self.engine.set_saved_state(uri.clone(), saved);
                            match util::uri_kind(uri) {
                                Some("track") => {
                                    if self.engine.liked_songs.intent(uri).is_some() {
                                        self.engine.liked_songs.confirm(uri, saved, true);
                                    } else if !saved {
                                        self.engine.library.liked.retain(|item| item.track.uri != *uri);
                                        if let Some(total) = self.engine.library.liked.total.as_mut() {
                                            *total = total.saturating_sub(1);
                                        }
                                    }
                                }
                                Some("album") => self.engine.library.albums.reset(),
                                Some("artist") => self.engine.library.artists.reset(),
                                Some("show") => self.engine.library.shows.reset(),
                                Some("episode") => self.engine.library.episodes.reset(),
                                _ => {}
                            }
                        }
                        let message = match (
                            current_uris.first().and_then(|uri| util::uri_kind(uri)),
                            saved,
                        ) {
                            (Some("track"), true) => self.engine.localize.t(self.engine.locale, "Added to Liked Songs"),
                            (Some("track"), false) => {
                                self.engine.localize.t(self.engine.locale, "Removed from Liked Songs")
                            }
                            (Some("artist"), true) => self.engine.localize.t(self.engine.locale, "Following artist"),
                            (Some("artist"), false) => self.engine.localize.t(self.engine.locale, "Unfollowed artist"),
                            (_, true) => self.engine.localize.t(self.engine.locale, "Saved to Your Library"),
                            (_, false) => self.engine.localize.t(self.engine.locale, "Removed from Your Library"),
                        };
                        if !current_uris.is_empty() {
                            self.engine.toast(message);
                        }
                    }
                    Err(error) => {
                        for uri in &current_uris {
                            self.engine.set_saved_state(uri.clone(), !saved);
                            self.engine.liked_songs.confirm(uri, saved, false);
                        }
                        if !current_uris.is_empty() {
                            self.engine.toast_error(
                                // Translators: {error} is an error message.
                                self.engine.localize.t(self.engine.locale, "Couldn't update your library: {error}")
                                    .replace("{error}", &error.to_string()),
                            );
                        }
                    }
                }
                if self.engine.liked_songs.cache_checked && self.engine.liked_songs.has_confirmed_changes() {
                    self.engine.sync_liked_songs();
                    self.engine.checkpoint_liked_songs(true);
                    self.engine.liked_recheck_at = Some(Instant::now() + Duration::from_secs(5));
                }
            }
            ApiResponse::Contains { uris, result } => {
                for uri in &uris {
                    self.engine.saved_pending.remove(uri);
                }
                if let Ok(flags) = result {
                    for (uri, flag) in uris.into_iter().zip(flags) {
                        if !self.engine.saved_writes.contains_key(&uri)
                            && self.engine.liked_songs.intent(&uri).is_none()
                        {
                            self.engine.set_saved_state(uri, flag);
                        }
                    }
                }
            }
            ApiResponse::SearchStarted {
                query,
                serial,
                split,
            } => {
                if serial == self.engine.search.serial && query == self.engine.search.committed {
                    self.engine.search.catalogue_pending = true;
                    self.engine.search.playlists_pending = split;
                }
            }
            ApiResponse::PlaySearch {
                query,
                serial,
                result,
            } => {
                if serial != self.engine.play_search_serial {
                    // Superseded by a newer search or by something the
                    // user played meanwhile.
                    return;
                }
                match result {
                    Ok(Some(uri)) => self.engine.actions.push(Action::PlaySearchMatch { uri, serial }),
                    Ok(None) => self.engine.toast(
                        self.engine.localize.t(
                            self.engine.locale,
                            // Translators: {query} is the words someone searched for.
                            "Nothing on Spotify matches \u{201c}{query}\u{201d}",
                        )
                        .replace("{query}", &query),
                    ),
                    Err(error) => self.engine.toast_error(
                        self.engine.localize.t(
                            self.engine.locale,
                            // Translators: {query} is the words someone searched for; {error} is an error message.
                            "Couldn't search for \u{201c}{query}\u{201d}: {error}",
                        )
                        .replace("{query}", &query)
                        .replace("{error}", &error.to_string()),
                    ),
                }
            }
            ApiResponse::SearchPlaylists {
                query,
                serial,
                result,
            } => {
                if serial != self.engine.search.serial || query != self.engine.search.committed {
                    return;
                }
                self.engine.search.playlists_pending = false;
                match result {
                    Ok(page) => {
                        self.engine.search.playlists = Some((serial, page));
                        self.engine.show_search_playlists();
                        self.engine.remember_search(&query);
                    }
                    Err(error) => self.engine.search_failed(&self.engine.localize.t(self.engine.locale, "Playlists"), error),
                }
            }
            ApiResponse::Search {
                query,
                serial,
                result,
            } => {
                if serial != self.engine.search.serial || query != self.engine.search.committed {
                    return;
                }
                self.engine.search.catalogue_pending = false;
                match result {
                    Ok(results) => {
                        let uris = results
                            .tracks
                            .iter()
                            .flat_map(|page| page.items.iter())
                            .map(|track| track.uri.clone())
                            .collect();
                        self.engine.request_contains(uris);
                        self.engine.remember_search(&query);
                        self.engine.search.results = Loadable::Loaded(results);
                        self.engine.search.results_serial = serial;
                        self.engine.show_search_playlists();
                    }
                    Err(error) => self.engine.search_failed(&self.engine.localize.t(self.engine.locale, "Search"), error),
                }
            }
            ApiResponse::Artist { id, result } => {
                if let Ok(artist) = &result {
                    if let Some(image) = pick_image(&artist.images, 64) {
                        self.engine.tint_for(Some(image));
                    }
                    if let Some(page) = self.engine.artist_pages.get_mut(&id)
                        && page.top_tracks.needs_load()
                    {
                        page.top_tracks = Loadable::Loading;
                        self.engine.backend
                            .api(ApiRequest::ArtistTopTracks { id: id.clone() });
                    }
                }
                if let Some(page) = self.engine.artist_pages.get_mut(&id) {
                    page.artist = match result {
                        Ok(artist) => Loadable::Loaded(artist),
                        Err(error) => Loadable::Failed(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                    };
                }
            }
            ApiResponse::ArtistTopTracks { id, result } => {
                if let Ok(tracks) = &result {
                    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();
                    self.engine.request_contains(uris);
                }
                if let Some(page) = self.engine.artist_pages.get_mut(&id) {
                    page.top_tracks = match result {
                        Ok(tracks) => Loadable::Loaded(tracks),
                        Err(error) => Loadable::Failed(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                    };
                }
            }
            ApiResponse::ArtistAlbums {
                id,
                groups,
                offset,
                result,
            } => {
                if let Ok(albums) = &result {
                    self.engine.request_album_types(albums.items.iter());
                }
                if let Some(page) = self.engine.artist_pages.get_mut(&id) {
                    let list = page.albums.entry(groups).or_default();
                    match result {
                        Ok(albums) => list.absorb(offset, albums),
                        Err(error) => list.fail(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                    }
                }
            }
            ApiResponse::RelatedArtists { id, result } => {
                if let Some(page) = self.engine.artist_pages.get_mut(&id) {
                    page.related = match result {
                        Ok(artists) => Loadable::Loaded(artists),
                        Err(error) => Loadable::Failed(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                    };
                }
            }
            ApiResponse::Album { id, result } => {
                // An earlier Web API header has no page generation. Once the
                // session resolves a countdown page, its preview and playable
                // cache own this album; a late generic header cannot replace it.
                if self.engine.album_pages
                    .get(&id)
                    .is_some_and(|page| page.prerelease.is_some())
                {
                    return;
                }
                let mut uris = Vec::new();
                if let Ok(album) = &result {
                    self.engine.request_album_types(std::iter::once(album));
                }
                if let Ok(album) = &result
                    && let Some(image) = pick_image(&album.images, 64)
                {
                    self.engine.tint_for(Some(image));
                }
                if let Some(page) = self.engine.album_pages.get_mut(&id) {
                    match result {
                        Ok(mut album) => {
                            if let Some(tracks) = album.tracks.take() {
                                uris = tracks.items.iter().map(|track| track.uri.clone()).collect();
                                page.tracks.absorb(0, tracks);
                            }
                            page.album = Loadable::Loaded(album);
                            if !page.tracks.loaded_once {
                                page.tracks.loading = true;
                                self.engine.backend.api(ApiRequest::AlbumTracks {
                                    id,
                                    offset: 0,
                                    generation: page.generation,
                                });
                            }
                        }
                        Err(error) => {
                            page.album = Loadable::Failed(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error))
                        }
                    }
                }
                self.engine.request_contains(uris);
            }
            ApiResponse::AlbumTracks {
                id,
                offset,
                generation,
                result,
            } => {
                if self.engine.album_pages
                    .get(&id)
                    .is_none_or(|page| page.generation != generation)
                {
                    return;
                }
                let mut uris = Vec::new();
                if let Some(page) = self.engine.album_pages.get_mut(&id) {
                    match result {
                        Ok(tracks) => {
                            uris = tracks.items.iter().map(|track| track.uri.clone()).collect();
                            page.tracks.absorb(offset, tracks);
                        }
                        Err(error) => page.tracks.fail(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                    }
                }
                self.engine.request_contains(uris);
                // A sorted table means the whole list, not the loaded part.
                if self.engine.table_sorts.contains_key(&Page::Album(id.clone())) {
                    self.engine.load_more(Page::Album(id));
                }
            }
            ApiResponse::AlbumQueueTracks {
                request,
                offset,
                result,
            } => {
                self.engine.receive_album_queue(request, offset, result);
            }
            ApiResponse::Show { id, result } => {
                if let Ok(show) = &result
                    && let Some(image) = pick_image(&show.images, 64)
                {
                    self.engine.tint_for(Some(image));
                }
                if let Some(page) = self.engine.show_pages.get_mut(&id) {
                    match result {
                        Ok(mut show) => {
                            if let Some(episodes) = show.episodes.take() {
                                page.episodes.absorb(0, episodes);
                            }
                            page.show = Loadable::Loaded(show);
                            if !page.episodes.loaded_once {
                                page.episodes.loading = true;
                                self.engine.backend.api(ApiRequest::ShowEpisodes { id, offset: 0 });
                            }
                        }
                        Err(error) => {
                            page.show = Loadable::Failed(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error))
                        }
                    }
                }
            }
            ApiResponse::ShowEpisodes { id, offset, result } => {
                if let Some(page) = self.engine.show_pages.get_mut(&id) {
                    match result {
                        Ok(episodes) => page.episodes.absorb(offset, episodes),
                        Err(error) => page.episodes.fail(friendly_api_error(self.engine.localize.as_ref(), self.engine.locale, &error)),
                    }
                }
            }
            ApiResponse::Track { id, result } => {
                self.engine.track_requests.remove(&id);
                match result {
                    Ok(track) => {
                        self.engine.remember_track_recording(&track);
                        self.engine.request_recording_candidates(&track);
                        if self.engine.liked_songs.update_track(&track) {
                            self.engine.sync_liked_songs();
                        }
                        self.engine.resolve_pasted_song(
                            &format!("spotify:track:{id}"),
                            Some(PlayableItem::Track(track.clone())),
                        );
                        self.engine.track_cache.insert(id.clone(), track);
                        self.engine.track_used.insert(id, Instant::now());
                    }
                    Err(error) => {
                        self.engine.resolve_pasted_song(&format!("spotify:track:{id}"), None);
                        if self.engine.pending_link.as_deref()
                            == Some(format!("spotify:track:{id}").as_str())
                        {
                            self.engine.pending_link = None;
                            self.engine.toast_error(
                                // Translators: {error} is an error message.
                                self.engine.localize.t(self.engine.locale, "Cannot open this song: {error}")
                                    .replace("{error}", &error.to_string()),
                            );
                        }
                    }
                }
            }
            // Only a link asks for an episode; its podcast's page is the
            // nearest thing to a page for it.
            ApiResponse::Episode { result, .. } => match result {
                Ok(episode) => match episode.show.filter(|show| !show.id.is_empty()) {
                    Some(show) => self.engine.open(Page::Show(show.id)),
                    None => self.engine.toast_error(self.engine.localize.t(
                        self.engine.locale,
                        "This episode's podcast is not on Spotify",
                    )),
                },
                Err(error) => self.engine.toast_error(
                    // Translators: {error} is an error message.
                    self.engine.localize.t(self.engine.locale, "Cannot open this episode: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            ApiResponse::Remote { action, result } => {
                if matches!(action, RemoteAction::Play | RemoteAction::Pause) {
                    self.engine.clear_play_pending();
                }
                match result {
                    Ok(()) => {
                        self.engine.remote_recheck_at = Some(Instant::now() + REMOTE_RECHECK);
                        self.engine.remote_command_at = Some(Instant::now());
                        if action == RemoteAction::Shuffle {
                            self.engine.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
                            if let Some(pending) = &mut self.engine.queue_shuffle_pending {
                                pending.at = Instant::now();
                            } else {
                                self.engine.note_shuffle_pending();
                            }
                        }
                    }
                    Err(error) => {
                        self.engine.optimistic_playing = None;
                        self.engine.pending_remote_position = None;
                        self.engine.pending_remote_volume = None;
                        if matches!(
                            action,
                            RemoteAction::Play | RemoteAction::Next | RemoteAction::Previous
                        ) && self.engine.intent_track.as_ref().is_some_and(|intent| {
                            matches!(intent.confirmation, TrackConfirmation::Remote { .. })
                        }) {
                            self.engine.intent_track = None;
                        }
                        let hint = if error.status() == Some(404) {
                            format!(
                                " {}",
                                self.engine.localize.t(
                                    self.engine.locale,
                                    "Choose a device from the devices menu first."
                                )
                            )
                        } else {
                            String::new()
                        };
                        self.engine.toast_error(format!(
                            "{}: {error}.{hint}",
                            remote_action_label(self.engine.localize.as_ref(), self.engine.locale, action)
                        ));
                    }
                }
                self.engine.poll_remote_soon();
            }
            ApiResponse::Transferred { device_id, result } => match result {
                Ok(()) => {
                    self.engine.selected_device = Some(device_id);
                    self.engine.show_devices = false;
                    self.engine.remote_command_at = Some(Instant::now());
                    self.engine.poll_remote_soon();
                    self.engine.refresh_devices();
                }
                Err(error) => self.engine.toast_error(
                    // Translators: {error} is an error message.
                    self.engine.localize.t(self.engine.locale, "Couldn't switch device: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            ApiResponse::QueueAdded { label: _, result } => match result {
                Ok(()) => {
                    // Refresh the queue after the optimistic update.
                    self.engine.refresh_queue(true);
                }
                Err(error) => self.engine.toast_error(
                    // Translators: {error} is an error message.
                    self.engine.localize.t(self.engine.locale, "Couldn't add to queue: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            ApiResponse::QueueBatchAdded {
                request,
                added,
                result,
            } => {
                if self.engine.pending_queue_batches.remove(&request).as_ref() != Some(&self.engine.target()) {
                    return;
                }
                if result.is_err() {
                    self.engine.remove_failed_queue_adds(request, added);
                }
                for addition in &mut self.engine.pending_queue_adds {
                    if addition.write.is_some_and(|(write, _)| write == request) {
                        addition.write = None;
                        addition.at = Instant::now();
                    }
                }
                if let Err(error) = result {
                    self.engine.toast_error(
                        // Translators: {error} is an error message.
                        self.engine.localize.t(self.engine.locale, "Couldn't add to queue: {error}")
                            .replace("{error}", &error.to_string()),
                    );
                }
                self.engine.refresh_queue(true);
            }
        }
    }

    fn next_pass(&self) -> Option<Duration> {
        let playing = self.engine.now_playing()
            .is_some_and(|now| now.playing)
            .then_some(Duration::from_millis(250));
        let pending = self.engine.any_play_pending()
            .then_some(Duration::from_millis(120));
        let polling = self.engine.is_connected()
            .then(|| self.connected_repaint_interval());
        [playing, pending, polling].into_iter().flatten().min()
    }






}

impl App {
    fn save_settings(&mut self) {
        if self.engine.settings.proxy_password_legacy {
            self.engine.settings_dirty = false;
            self.engine.last_settings_save = Instant::now();
            // Do not erase the sole surviving legacy password when the native
            // store is locked. Its successful callback owns retrying this save.
            return;
        }
        self.engine.settings_dirty = false;
        self.engine.last_settings_save = Instant::now();
        if self.engine.offline {
            // Demo data must never overwrite the person's real preferences.
            return;
        }
        let mut saved = self.engine.settings.clone();
        self.applied_proxy_preferences.apply_to(&mut saved);
        if let Some(text) = saved.encode() {
            self.engine.files.write(&self.engine.dirs.settings_file(), text.into_bytes());
        }
    }

    fn regular_poll_interval(&self) -> Duration {
        let floor = if self.engine.remote_command_at
            .is_some_and(|at| at.elapsed() < REMOTE_COMMAND_WINDOW)
        {
            Duration::ZERO
        } else {
            REMOTE_POLL_MIN
        };
        self.connected_repaint_interval().max(floor)
    }

    fn save_session(&mut self) {
        self.engine.session_dirty = false;
        self.engine.last_session_save = Instant::now();
        if let Some(now) = self.engine.now_playing() {
            self.engine.resume_context = self.engine.playing_context_uri();
            self.engine.resume_track = Some(now.uri.clone());
            self.engine.resume_position_ms = now.position_ms;
        }
        if !self.engine.offline {
            let session = SessionState {
                last_page: Some(self.engine.page().encode()),
                recent_contexts: self.engine.recent_contexts.clone(),
                last_context: self.engine.resume_context.clone(),
                last_track: self.engine.resume_track.clone(),
                last_position_ms: self.engine.resume_position_ms,
                collapsed_folders: self.engine.collapsed_folders.clone(),
                rootlist: self.engine.rootlist_cache.clone(),
                last_added_queue: if self.engine.resume_queue.is_empty() {
                    self.engine.manual_queue.clone()
                } else {
                    // Never resumed this session; the owed queue carries over.
                    self.engine.resume_queue.clone()
                },
                last_queue_rows: self.engine.queue
                    .get()
                    .map(|queue| queue.queue.iter().take(30).cloned().collect())
                    .unwrap_or_default(),
                shuffle_on: self.engine.shuffle_wanted,
                sorts: self.engine.table_sorts
                    .iter()
                    .map(|(page, sort)| (page.encode(), *sort))
                    .collect(),
                window_size: self.window_geometry.size,
                window_pos: self.window_geometry.position,
                window_maximized: self.window_geometry.maximized,
                queue_open: Some(self.engine.show_queue_panel),
                queue_tab: Some(self.engine.queue_tab.encode().to_string()),
                winamp_pos: self.winamp.geometry.position,
                milkdrop_pos: self.milkdrop_pos,
                lyrics_fullscreen_from: self.engine.lyrics_fullscreen.map(|fullscreen| {
                    crate::settings::WindowMode {
                        fullscreen,
                        maximized: self.engine.lyrics_restore_maximized,
                    }
                }),
            };
            if let Some(text) = session.encode() {
                self.engine.files.write(&self.engine.dirs.session_file(), text.into_bytes());
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::api::models::{
        Episode, Image, Page as ApiPage, ResumePoint, SavedEpisode, SavedTrack, SearchResults,
    };

    /// #623: the missing-output message the sink reports is the one the
    /// catalogues translate.
    #[test]
    fn a_missing_audio_output_is_reported_in_the_users_language() {
        assert_eq!(
            engine_error_text(&crate::i18n::GettextLocalize, Locale::English, crate::sink::NO_DEVICE),
            crate::sink::NO_DEVICE
        );
        let german = engine_error_text(&crate::i18n::GettextLocalize, Locale::German, crate::sink::NO_DEVICE);
        assert_ne!(german, crate::sink::NO_DEVICE);
        assert!(!german.is_empty());
        assert_eq!(
            engine_error_text(&crate::i18n::GettextLocalize, Locale::German, "Something else"),
            "Something else"
        );
    }

    #[test]
    fn middle_clicking_a_playlist_row_autoscrolls_only_on_windows_by_default() {
        middle_click_a_playlist_row(false);
    }

    /// A narrow window takes width from the side panels before the top bar
    /// runs out of room (#624), and the widths the listener chose come back
    /// once the window widens. With the Queue open, the window's minimum
    /// grows to the panels' least widths and the page's.
    #[test]
    fn side_panels_give_the_top_bar_its_room_and_keep_their_widths() {
        fn draw(ctx: &egui::Context, app: &mut App, width: f32) -> Vec<egui::ViewportCommand> {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 700.0),
                    )),
                    ..Default::default()
                },
                |ui| app.frame_ui(ui),
            );
            output.textures_delta.clear();
            output
                .viewport_output
                .remove(&egui::ViewportId::ROOT)
                .map(|viewport| viewport.commands)
                .unwrap_or_default()
        }
        let panel = |ctx: &egui::Context, id: &str| {
            egui::containers::panel::PanelState::load(ctx, egui::Id::new(id))
                .unwrap()
                .size()
                .x
        };
        let ctx = egui::Context::default();
        let mut app = test_app("panels-give-way");
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.engine.settings.sidebar_width = 400.0;
        app.engine.settings.queue_width = 500.0;
        app.engine.show_queue_panel = true;

        let mut commands = Vec::new();
        for _ in 0..3 {
            commands.extend(draw(&ctx, &mut app, 1000.0));
        }
        let least = crate::ui::topbar::least_width(&ctx);
        let sidebar = panel(&ctx, "sidebar");
        let queue = panel(&ctx, "queue-panel");
        assert!(
            sidebar + queue + least <= 1000.5,
            "the page keeps {} of the {least} points its bar needs",
            1000.0 - sidebar - queue
        );
        assert!(sidebar >= 210.0 && queue >= crate::theme::SIDE_PANEL_MIN_WIDTH);
        assert_eq!(app.engine.settings.sidebar_width, 400.0);
        assert_eq!(app.engine.settings.queue_width, 500.0);
        let min = (210.0 + crate::theme::SIDE_PANEL_MIN_WIDTH + least).round();
        assert!(
            commands.iter().any(|command| matches!(
                command,
                egui::ViewportCommand::MinInnerSize(size) if size.x == min
            )),
            "the window's minimum makes room for the Queue: {commands:?}"
        );

        for _ in 0..2 {
            draw(&ctx, &mut app, 1800.0);
        }
        assert_eq!(panel(&ctx, "sidebar"), 400.0);
        assert_eq!(panel(&ctx, "queue-panel"), 500.0);

        // Closing the Queue gives the window its usual minimum back.
        app.engine.show_queue_panel = false;
        let commands = draw(&ctx, &mut app, 1800.0);
        assert!(commands.iter().any(|command| matches!(
            command,
            egui::ViewportCommand::MinInnerSize(size)
                if size.x == crate::window::MAIN_MIN_SIZE[0]
        )));
    }

    /// Linux autoscrolls once the listener turns it on; macOS never does.
    #[test]
    fn middle_clicking_a_playlist_row_autoscrolls_on_linux_when_chosen() {
        middle_click_a_playlist_row(true);
    }

    /// Middle-clicks a real playlist row, moves over the queue, and checks
    /// that the list scrolls exactly where autoscroll is on, and that the
    /// click never plays the row.
    fn middle_click_a_playlist_row(chosen: bool) {
        use egui::accesskit::Role;
        let on = crate::autoscroll::enabled(chosen);
        fn draw(
            ctx: &egui::Context,
            app: &mut App,
            time: u32,
            events: Vec<egui::Event>,
        ) -> egui::accesskit::TreeUpdate {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    time: Some(time as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| app.frame_ui(ui),
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        }
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = test_app(if chosen {
            "autoscroll-playlist-row-chosen"
        } else {
            "autoscroll-playlist-row"
        });
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.engine.settings.middle_click_autoscroll = chosen;
        app.engine.open(Page::Playlist("pl1".into()));
        app.engine.show_queue_panel = true;
        let playing = app.engine.now_playing().unwrap().uri.clone();
        draw(&ctx, &mut app, 0, vec![]);
        let tree = draw(&ctx, &mut app, 1, vec![]);
        let (id, node) = tree
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == Role::Button
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Play ") && label.contains(','))
                    && node.bounds().is_some_and(|rect| {
                        rect.x0 > 250.0
                            && rect.x0 < 700.0
                            && rect.width() > 400.0
                            && rect.y0 > 60.0
                            && rect.y1 < 700.0
                    })
            })
            .expect("a visible playlist row");
        let id = *id;
        let before = node.bounds().unwrap();
        let anchor = egui::pos2(before.x0 as f32 + 120.0, before.y0 as f32 + 12.0);
        draw(
            &ctx,
            &mut app,
            2,
            vec![
                egui::Event::PointerMoved(anchor),
                egui::Event::PointerButton {
                    pos: anchor,
                    button: egui::PointerButton::Middle,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(
            app.autoscroll.active(),
            on,
            "only an enabled autoscroll arms the real row's scroll area"
        );
        draw(
            &ctx,
            &mut app,
            3,
            vec![egui::Event::PointerButton {
                pos: anchor,
                button: egui::PointerButton::Middle,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        for frame in 4..8 {
            draw(
                &ctx,
                &mut app,
                frame,
                vec![egui::Event::PointerMoved(egui::pos2(1100.0, 680.0))],
            );
        }
        let tree = draw(&ctx, &mut app, 8, vec![]);
        let after = tree
            .nodes
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .expect("the same displayed occurrence")
            .1
            .bounds()
            .unwrap();
        if on {
            assert!(
                after.y0 < before.y0,
                "the playlist must scroll while the pointer is over Queue"
            );
        } else {
            assert_eq!(after.y0, before.y0, "middle-click must not move the list");
        }
        assert_eq!(app.engine.now_playing().unwrap().uri, playing);
        assert_eq!(app.autoscroll.active(), on);
    }

    /// A press on a link or a button inside a row starts autoscroll like
    /// one on the row itself: none of them uses the middle button.
    #[test]
    fn middle_clicking_a_link_or_button_in_a_playlist_row_autoscrolls() {
        use egui::accesskit::Role;
        let on = crate::autoscroll::enabled(true);
        fn draw(
            ctx: &egui::Context,
            app: &mut App,
            time: u32,
            events: Vec<egui::Event>,
        ) -> egui::accesskit::TreeUpdate {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    time: Some(time as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| app.frame_ui(ui),
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        }
        for (role, label) in [
            (Role::Link, None),
            (Role::Button, Some("Save to Liked Songs")),
            (Role::Button, Some("More")),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut app = test_app("autoscroll-row-controls");
            app.attach(&ctx);
            crate::demo::populate(&mut app);
            app.engine.settings.middle_click_autoscroll = true;
            app.engine.open(Page::Playlist("pl1".into()));
            draw(&ctx, &mut app, 0, vec![]);
            let tree = draw(&ctx, &mut app, 1, vec![]);
            let control = tree
                .nodes
                .iter()
                .filter_map(|(_, node)| Some((node, node.bounds()?)))
                .find(|(node, rect)| {
                    node.role() == role
                        && label.is_none_or(|label| node.label() == Some(label))
                        && rect.x0 > 300.0
                        && rect.y0 > 300.0
                        && rect.y1 < 650.0
                })
                .map(|(_, rect)| egui::pos2(rect.x0 as f32 + 4.0, (rect.y0 + rect.y1) as f32 / 2.0))
                .unwrap_or_else(|| panic!("a visible {role:?} {label:?} in a row"));
            draw(&ctx, &mut app, 2, vec![egui::Event::PointerMoved(control)]);
            draw(
                &ctx,
                &mut app,
                3,
                vec![egui::Event::PointerButton {
                    pos: control,
                    button: egui::PointerButton::Middle,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(app.autoscroll.active(), on, "{role:?} {label:?}");
            app.engine.backend.shutdown();
        }
    }

    #[test]
    fn autoscroll_updates_the_real_lyrics_and_skinned_playlist_without_changing_playback() {
        for (skinned, chosen) in [(false, false), (true, false), (false, true), (true, true)] {
            let on = crate::autoscroll::enabled(chosen);
            let ctx = egui::Context::default();
            let mut app = test_app(match (skinned, chosen) {
                (false, false) => "autoscroll-lyrics",
                (true, false) => "autoscroll-skin",
                (false, true) => "autoscroll-lyrics-chosen",
                (true, true) => "autoscroll-skin-chosen",
            });
            app.attach(&ctx);
            crate::demo::populate(&mut app);
            app.engine.settings.middle_click_autoscroll = chosen;
            app.engine.show_queue_panel = false;
            app.engine.show_lyrics_panel = !skinned;
            app.engine.settings.winamp_window = skinned;
            app.engine.settings.playlist_open = skinned;
            app.engine.settings.skin_scale = Some(2);
            if let Loadable::Loaded(queue) = &mut app.engine.queue {
                queue.queue = queue.queue.iter().cycle().take(80).cloned().collect();
            }
            app.engine.lyrics = Loadable::Loaded(Some(crate::lyrics::Lyrics {
                lines: (0..80)
                    .map(|i| crate::lyrics::Line {
                        at_ms: Some(i * 5000),
                        text: format!("Autoscroll lyric line {i}"),
                    })
                    .collect(),
                synced: true,
                instrumental: false,
            }));
            if let Some(remote) = &mut app.engine.remote {
                remote.state.is_playing = false;
                remote.state.progress_ms = Some(0);
            }
            let playing = app.engine.now_playing().unwrap().uri.clone();
            let mut frame = 0;
            let mut draw = |app: &mut App, events: Vec<egui::Event>| {
                frame += 1;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            if skinned {
                                egui::vec2(550.0, 580.0)
                            } else {
                                egui::vec2(1280.0, 800.0)
                            },
                        )),
                        time: Some(frame as f64 / 60.0),
                        events,
                        ..Default::default()
                    },
                    |ui| app.frame_ui(ui),
                );
                output.textures_delta.clear();
            };
            draw(&mut app, vec![]);
            draw(&mut app, vec![]);
            let anchor = if skinned {
                ctx.read_response(egui::Id::new(("playlist-row", 0_usize)))
                    .unwrap()
                    .rect
                    .center()
            } else {
                egui::pos2(1120.0, 100.0)
            };
            let press = |button, pressed| egui::Event::PointerButton {
                pos: anchor,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            draw(
                &mut app,
                vec![
                    egui::Event::PointerMoved(anchor),
                    press(egui::PointerButton::Middle, true),
                ],
            );
            assert_eq!(
                app.autoscroll.active(),
                on,
                "real surface, skinned={skinned}"
            );
            if !skinned {
                assert_eq!(app.engine.lyrics_following, !on);
            }
            draw(&mut app, vec![press(egui::PointerButton::Middle, false)]);
            for _ in 0..5 {
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(anchor + egui::vec2(0.0, 120.0))],
                );
            }
            if skinned {
                if on {
                    assert!(app.winamp.playlist_scroll > 0);
                } else {
                    assert_eq!(app.winamp.playlist_scroll, 0);
                }
                assert!(
                    app.winamp.playlist_selection.is_empty(),
                    "middle-click must not select a row"
                );
            } else {
                assert_eq!(app.engine.lyrics_following, !on);
            }
            assert_eq!(app.engine.now_playing().unwrap().uri, playing);
            draw(&mut app, vec![press(egui::PointerButton::Primary, true)]);
            assert!(!app.autoscroll.active());
            draw(&mut app, vec![press(egui::PointerButton::Primary, false)]);
            if skinned {
                // After cancelling, a normal click selects the visible row.
                draw(&mut app, vec![egui::Event::PointerMoved(anchor)]);
                draw(&mut app, vec![press(egui::PointerButton::Primary, true)]);
                draw(&mut app, vec![press(egui::PointerButton::Primary, false)]);
                assert_eq!(
                    app.winamp.playlist_selection.len(),
                    1,
                    "ordinary row selection must still work after cancellation"
                );
            }
            app.engine.backend.shutdown();
        }
    }

    #[test]
    fn shift_wheel_moves_the_shelf_without_scrolling_the_page() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut shelf_left = 0.0;
        let mut initial_left = 0.0;
        let mut page_offset = 0.0;
        for frame in 0..6 {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            };
            input
                .events
                .push(egui::Event::PointerMoved(egui::pos2(100.0, 70.0)));
            if frame == 2 {
                input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -3.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::SHIFT,
                });
            }
            let mut output = ctx.run_ui(input, |ui| {
                app.scrolling.apply(ui.ctx());
                let page = egui::ScrollArea::vertical().show(ui, |ui| {
                    crate::ui::widgets::shelf(
                        ui,
                        &app.palette,
                        "wheel-test-shelf",
                        "Shelf",
                        |ui| {
                            shelf_left = ui.allocate_space(egui::vec2(1600.0, 100.0)).1.left();
                        },
                    );
                    ui.allocate_space(egui::vec2(100.0, 1200.0));
                });
                page_offset = page.state.offset.y;
            });
            output.textures_delta.clear();
            if frame == 0 {
                initial_left = shelf_left;
            }
        }
        assert!(shelf_left < initial_left, "Shift+wheel must move the shelf");
        assert_eq!(page_offset, 0.0, "the enclosing page must stay put");
    }

    /// A song started outside a playlist must turn off the playlist's
    /// sidebar light at once, even while Spotify still reports the old
    /// context from before the click.
    #[test]
    fn a_plain_song_clears_the_sidebar_context() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.assumed_context = Some(AssumedContext {
            uri: "spotify:playlist:sidebar".into(),
            shuffle: None,
            at: Instant::now(),
        });
        app.engine.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                context: Some(crate::api::models::Context {
                    uri: "spotify:playlist:sidebar".into(),
                    ..Default::default()
                }),
                is_playing: true,
                ..Default::default()
            },
            received_at: Instant::now(),
        });

        app.apply(
            Action::PlayUris {
                uris: vec!["spotify:track:standalone".into()],
                index: 0,
            },
            &ctx,
        );

        assert_eq!(app.engine.playing_context_uri(), None);
    }

    /// The playlist just started stays lit after the first seconds even
    /// while the last poll, taken before the click, still names what the
    /// phone was playing; only a poll from after the click may say
    /// otherwise.
    #[test]
    fn a_poll_from_before_the_click_does_not_move_the_sidebar_light() {
        // #given a poll from the phone's playlist, then a playlist started here
        let mut app = headless_app();
        app.engine.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                context: Some(crate::api::models::Context {
                    uri: "spotify:playlist:phone".into(),
                    ..Default::default()
                }),
                is_playing: true,
                ..Default::default()
            },
            received_at: Instant::now() - Duration::from_secs(30),
        });
        app.engine.assumed_context = Some(AssumedContext {
            uri: "spotify:playlist:here".into(),
            shuffle: None,
            at: Instant::now() - ASSUMED_CONTEXT_HOLD - Duration::from_secs(1),
        });
        app.engine.optimistic_playing = Some((true, Instant::now()));

        // #then the hold is over, yet the stale poll does not win
        assert_eq!(
            app.engine.playing_context_uri().as_deref(),
            Some("spotify:playlist:here")
        );

        // #when a poll taken after the click still reports the phone's playlist
        app.engine.remote.as_mut().expect("a snapshot").received_at = Instant::now();

        // #then Spotify's word from after the click stands
        assert_eq!(
            app.engine.playing_context_uri().as_deref(),
            Some("spotify:playlist:phone")
        );
    }

    /// A song the Web API had greyed out stays greyed out when the session
    /// reads the rows, at every place it recurs, whether the Web API's word
    /// came from an earlier page or from the disk cache of a visit the
    /// playlist has changed since. Rows it never described stay unknown,
    /// and a song it later calls playable is no longer greyed out.
    #[test]
    fn a_session_read_keeps_a_songs_known_unavailability() {
        let mut app = headless_app();
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        let row = |uri: &str, is_playable: Option<bool>| crate::api::models::PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                uri: uri.into(),
                is_playable,
                ..Default::default()
            })),
            ..Default::default()
        };
        let items =
            |id: &str, rows: Vec<crate::api::models::PlaylistItem>| ApiResponse::PlaylistItems {
                id: id.into(),
                offset: 0,
                generation: 0,
                result: Ok(crate::api::models::Page {
                    total: rows.len() as u32,
                    limit: 50,
                    items: rows,
                    ..Default::default()
                }),
            };
        let rows = |app: &App, id: &str| {
            app.engine.playlist_pages[id]
                .items
                .items
                .iter()
                .map(|item| match item.playable() {
                    Some(PlayableItem::Track(track)) => track.is_playable,
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        // The Web API's word from an earlier page of the same list.
        app.engine.playlist_pages
            .insert("pl1".into(), PlaylistPage::default());
        app.handle_api(items(
            "pl1",
            vec![
                row("spotify:track:gone", Some(false)),
                row("spotify:track:fine", Some(true)),
                row("spotify:track:gone", Some(false)),
            ],
        ));
        app.handle_api(items(
            "pl1",
            vec![
                row("spotify:track:gone", None),
                row("spotify:track:fine", None),
                row("spotify:track:new", None),
                row("spotify:track:gone", None),
            ],
        ));
        assert_eq!(rows(&app, "pl1"), [Some(false), None, None, Some(false)]);

        // The Web API's word from the disk cache of another visit, read
        // after the session page arrived and never adopted, the playlist
        // having changed since.
        app.engine.playlist_pages.insert(
            "pl2".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "pl2".into(),
                    snapshot_id: Some("now".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        app.handle_api(items(
            "pl2",
            vec![
                row("spotify:track:other", None),
                row("spotify:track:fine", None),
            ],
        ));
        app.engine.receive_playlist_cache(
            "alice",
            "pl2",
            0,
            Some(PlaylistCache {
                snapshot: "then".into(),
                items: vec![row("spotify:track:other", Some(false))],
                total: 1,
                next_offset: None,
                appendable: false,
            }),
        );
        assert_eq!(
            rows(&app, "pl2"),
            [Some(false), None],
            "the cache's word reaches the rows already shown"
        );
        assert!(
            app.engine.playlist_pages["pl2"].pending_cache.is_none(),
            "though the stale cache itself is not adopted"
        );

        // A song the Web API later calls playable is no longer greyed out.
        app.handle_api(items("pl2", vec![row("spotify:track:other", Some(true))]));
        app.handle_api(items("pl2", vec![row("spotify:track:other", None)]));
        assert_eq!(rows(&app, "pl2"), [None]);
    }

    fn availability_row(playable: Option<bool>) -> PlaylistItem {
        PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                uri: "spotify:track:availability".into(),
                is_playable: playable,
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    fn receive_availability_rows(app: &mut App, id: &str, playable: Option<bool>) {
        app.engine.playlist_pages
            .entry(id.into())
            .or_insert_with(|| PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: id.into(),
                    snapshot_id: Some("now".into()),
                    ..Default::default()
                }),
                ..Default::default()
            });
        app.handle_api(ApiResponse::PlaylistItems {
            id: id.into(),
            offset: 0,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![availability_row(playable)],
                total: 1,
                limit: 50,
                ..Default::default()
            }),
        });
    }

    fn shown_availability(app: &mut App, id: &str) -> Option<bool> {
        let page = &app.engine.playlist_pages[id];
        let generation = page.generation;
        let revision = page.items.revision;
        let rows = page
            .items
            .items
            .iter()
            .filter_map(|row| row.playable().cloned().map(|item| (item, None, None)))
            .collect();
        let rows = crate::ui::collection::cached_table_items(
            app,
            Page::Playlist(id.into()),
            generation,
            revision,
            app.engine.user_names_revision,
            || rows,
        );
        match &rows[0].0 {
            PlayableItem::Track(track) => track.is_playable,
            _ => panic!("a track row"),
        }
    }

    fn old_availability_cache(playable: bool) -> Option<PlaylistCache> {
        Some(PlaylistCache {
            snapshot: "then".into(),
            items: vec![availability_row(Some(playable))],
            total: 1,
            next_offset: None,
            appendable: false,
        })
    }

    #[test]
    fn playlist_availability_does_not_follow_an_account_switch() {
        let mut app = headless_app();
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        receive_availability_rows(&mut app, "pl1", Some(false));
        app.engine.handle_auth(AuthStatus::SignedOut);
        app.engine.handle_auth(AuthStatus::Connected {
            username: "bob".into(),
        });
        app.engine.user = Some(User {
            id: "bob".into(),
            ..Default::default()
        });
        receive_availability_rows(&mut app, "pl1", None);
        app.engine.receive_playlist_cache("alice", "pl1", 0, old_availability_cache(false));
        assert_eq!(shown_availability(&mut app, "pl1"), None);
    }

    #[test]
    fn playlist_availability_prefers_fresh_answers_to_late_disk_caches() {
        for fresh in [true, false] {
            let mut app = headless_app();
            app.engine.user = Some(User {
                id: "alice".into(),
                ..Default::default()
            });
            receive_availability_rows(&mut app, "pl1", Some(fresh));
            receive_availability_rows(&mut app, "pl2", None);
            app.engine.receive_playlist_cache("alice", "pl2", 0, old_availability_cache(!fresh));
            receive_availability_rows(&mut app, "pl3", None);
            assert_eq!(
                shown_availability(&mut app, "pl3"),
                (!fresh).then_some(false)
            );
        }
    }

    #[test]
    fn playlist_availability_from_disk_reaches_rows_already_drawn() {
        let mut app = headless_app();
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        receive_availability_rows(&mut app, "pl1", None);
        assert_eq!(shown_availability(&mut app, "pl1"), None);
        app.engine.receive_playlist_cache("alice", "pl1", 0, old_availability_cache(false));
        assert_eq!(shown_availability(&mut app, "pl1"), Some(false));
    }

    #[test]
    fn playlist_availability_stays_fresh_when_a_cached_prefix_is_adopted() {
        let mut app = headless_app();
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        receive_availability_rows(&mut app, "pl1", Some(true));
        app.engine.playlist_pages.insert(
            "pl2".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "pl2".into(),
                    snapshot_id: Some("then".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        app.engine.receive_playlist_cache("alice", "pl2", 0, old_availability_cache(false));
        assert_eq!(shown_availability(&mut app, "pl2"), Some(true));
    }

    /// Saving the edit dialog sends the public flag only when its switch
    /// was used; a playlist nothing has described keeps whatever it was.
    #[test]
    fn saving_playlist_details_leaves_an_unknown_public_flag_alone() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        let save = |app: &mut App, public| {
            app.apply(
                Action::UpdatePlaylist {
                    id: "pl1".into(),
                    name: "Renamed".into(),
                    description: String::new(),
                    public,
                },
                &ctx,
            );
            match app.engine.backend.take_playlist_add_requests().as_slice() {
                [ApiRequest::UpdatePlaylist { public, .. }] => *public,
                sent => panic!("{sent:?}"),
            }
        };
        assert_eq!(save(&mut app, None), None);
        assert_eq!(save(&mut app, Some(false)), Some(false));
    }

    /// Saving sends only what the dialog changed. Spotify refuses an empty
    /// description, so clearing one keeps it and says why instead of
    /// failing a rename saved at the same time.
    /// Spotify's playlist order can't be changed by apps, so dragging a
    /// playlist in it switches to the local order and says why, once.
    #[test]
    fn dragging_in_spotify_order_switches_to_a_local_order_and_says_why() {
        use crate::settings::{LibraryShelf, LibrarySort};
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.engine.settings
            .library_sort
            .insert(LibraryShelf::Playlists, LibrarySort::Spotify);
        let arrange = |app: &mut App| {
            app.apply(
                Action::ArrangeLibrary {
                    pinned: Vec::new(),
                    playlist_order: Some(vec![
                        "spotify:playlist:b".into(),
                        "spotify:playlist:a".into(),
                    ]),
                },
                &ctx,
            );
        };
        let toasts = app.engine.toasts.len();
        arrange(&mut app);
        assert_eq!(
            app.engine.settings.library_sort.get(&LibraryShelf::Playlists),
            Some(&LibrarySort::Local)
        );
        assert_eq!(app.engine.toasts.len(), toasts + 1);
        assert!(app.engine.toasts.last().unwrap().message.contains("Spotify"));

        arrange(&mut app);
        assert_eq!(
            app.engine.toasts.len(),
            toasts + 1,
            "already local, nothing to explain"
        );
    }

    /// The title bar choice is saved and, because decorations are fixed when
    /// a window is created, replaces only the main window's native window.
    /// The mini player picks it up the next time the main window opens.
    #[test]
    fn choosing_the_custom_title_bar_recreates_only_the_main_window() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.apply(Action::SetCustomTitlebar(true), &ctx);
        assert!(app.engine.settings.custom_titlebar);
        assert!(app.switch_intent, "the main window is replaced");
        assert_eq!(
            fastframe_titlebar::enabled(),
            cfg!(any(windows, target_os = "linux"))
        );

        app.switch_intent = false;
        app.apply(Action::SetCustomTitlebar(true), &ctx);
        assert!(!app.switch_intent, "an unchanged choice keeps the window");

        app.engine.settings.winamp_window = true;
        app.apply(Action::SetCustomTitlebar(false), &ctx);
        assert!(!app.engine.settings.custom_titlebar);
        assert!(!app.switch_intent, "the mini player is not the main window");
        assert!(!fastframe_titlebar::enabled());
    }

    #[test]
    fn saving_playlist_details_sends_only_what_changed() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.engine.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            name: "Mix".into(),
            description: Some("<b>Old</b> notes".into()),
            public: Some(true),
            ..Playlist::default()
        }]);
        let save = |app: &mut App, name: &str, description: &str, public| {
            app.apply(
                Action::UpdatePlaylist {
                    id: "pl1".into(),
                    name: name.into(),
                    description: description.into(),
                    public,
                },
                &ctx,
            );
            match app.engine.backend.take_playlist_add_requests().as_slice() {
                [] => None,
                [
                    ApiRequest::UpdatePlaylist {
                        name,
                        description,
                        public,
                        ..
                    },
                ] => Some((name.clone(), description.clone(), *public)),
                sent => panic!("{sent:?}"),
            }
        };

        assert_eq!(
            save(&mut app, "Renamed", "Old notes", Some(true)),
            Some((Some("Renamed".into()), None, None)),
            "an untouched description is not sent back without its HTML"
        );
        assert_eq!(
            save(&mut app, "Mix", "New notes", Some(true)),
            Some((None, Some("New notes".into()), None))
        );
        assert_eq!(
            save(&mut app, "Mix", "Old notes", Some(false)),
            Some((None, None, Some(false)))
        );

        app.engine.playlist_busy = false;
        let toasts = app.engine.toasts.len();
        assert_eq!(save(&mut app, "Mix", "", Some(true)), None);
        assert!(!app.engine.playlist_busy, "nothing was sent, so nothing waits");
        assert_eq!(app.engine.toasts.len(), toasts + 1);
        assert_eq!(
            save(&mut app, "Renamed", "", Some(true)),
            Some((Some("Renamed".into()), None, None)),
            "clearing the description does not fail the rename"
        );
    }

    /// A header read over the streaming session carries no public flag,
    /// and the edit dialog fills its switch from it, so the library list's
    /// answer stands in.
    #[test]
    fn a_header_without_a_public_flag_takes_the_library_lists() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            public: Some(true),
            ..Playlist::default()
        }]);
        app.engine.playlist_pages.insert(
            "pl1".into(),
            PlaylistPage {
                generation: 1,
                ..Default::default()
            },
        );
        app.handle_api(ApiResponse::Playlist {
            id: "pl1".into(),
            generation: 1,
            result: Ok(Playlist {
                id: "pl1".into(),
                name: "Mine".into(),
                ..Playlist::default()
            }),
        });
        let playlist = app.engine.playlist_pages["pl1"].playlist.get().unwrap();
        assert_eq!(playlist.public, Some(true));
        assert_eq!(playlist.name, "Mine", "the rest is Spotify's answer");

        // Spotify's own answer outranks the list, and a playlist the list
        // does not hold stays unknown rather than guessed.
        app.handle_api(ApiResponse::Playlist {
            id: "pl1".into(),
            generation: 1,
            result: Ok(Playlist {
                id: "pl1".into(),
                public: Some(false),
                ..Playlist::default()
            }),
        });
        assert_eq!(
            app.engine.playlist_pages["pl1"].playlist.get().unwrap().public,
            Some(false)
        );
        app.engine.playlist_pages.insert(
            "pl2".into(),
            PlaylistPage {
                generation: 1,
                ..Default::default()
            },
        );
        app.handle_api(ApiResponse::Playlist {
            id: "pl2".into(),
            generation: 1,
            result: Ok(Playlist {
                id: "pl2".into(),
                ..Playlist::default()
            }),
        });
        assert_eq!(
            app.engine.playlist_pages["pl2"].playlist.get().unwrap().public,
            None
        );

        // The list can arrive after the header: the page takes the flag
        // then, and a flag Spotify already gave stays.
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![
                    Playlist {
                        id: "pl1".into(),
                        public: Some(true),
                        ..Playlist::default()
                    },
                    Playlist {
                        id: "pl2".into(),
                        public: Some(true),
                        ..Playlist::default()
                    },
                ],
                ..Default::default()
            }),
        });
        assert_eq!(
            app.engine.playlist_pages["pl2"].playlist.get().unwrap().public,
            Some(true)
        );
        assert_eq!(
            app.engine.playlist_pages["pl1"].playlist.get().unwrap().public,
            Some(false)
        );
    }

    /// A page of the library's playlists, `limit` 2, that says whether
    /// more follow.
    fn playlist_page(ids: &[&str], offset: u32, total: u32) -> crate::api::models::Page<Playlist> {
        crate::api::models::Page {
            items: ids
                .iter()
                .map(|id| Playlist {
                    id: (*id).into(),
                    uri: format!("spotify:playlist:{id}"),
                    ..Playlist::default()
                })
                .collect(),
            total,
            limit: 2,
            offset,
            next: (offset + (ids.len() as u32) < total).then(|| "more".to_string()),
        }
    }

    fn listed_playlists(app: &App) -> Option<Vec<String>> {
        app.engine.library.playlists.get().map(|playlists| {
            playlists
                .iter()
                .map(|playlist| playlist.id.clone())
                .collect::<Vec<_>>()
        })
    }

    fn playlist_ids(ids: &[&str]) -> Option<Vec<String>> {
        Some(ids.iter().map(|id| (*id).to_string()).collect())
    }

    /// Spotify's Web API lists some of its own playlists, "This Is" ones
    /// among them, with no name, cover or songs, which drew a blank sidebar
    /// row. The streaming session reads their headers whole: each row takes
    /// its header as it arrives, is asked about once, and a reloaded list
    /// fills at once.
    #[test]
    fn a_playlist_the_library_lists_blank_takes_its_session_header() {
        let mut app = headless_app();
        let listed = || {
            Ok(crate::api::models::Page {
                items: vec![
                    Playlist {
                        id: "blank".into(),
                        uri: "spotify:playlist:blank".into(),
                        description: Some(String::new()),
                        items_count: Some(TrackCount { total: 0 }),
                        ..Playlist::default()
                    },
                    Playlist {
                        id: "later".into(),
                        uri: "spotify:playlist:later".into(),
                        ..Playlist::default()
                    },
                    Playlist {
                        id: "named".into(),
                        name: "Mix".into(),
                        ..Playlist::default()
                    },
                ],
                ..Default::default()
            })
        };
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: listed(),
        });
        assert_eq!(
            app.engine.playlist_headers_requested,
            HashSet::from(["blank".to_string(), "later".to_string()]),
            "only the blank entries are asked about"
        );

        let cover = Image {
            url: "https://i.scdn.co/image/this-is".into(),
            width: Some(300),
            height: None,
        };
        app.handle_backend_events(vec![Event::PlaylistHeader(Box::new(Playlist {
            id: "blank".into(),
            name: "This Is TUYU".into(),
            description: Some("The essential tracks.".into()),
            images: vec![cover.clone()],
            items_count: Some(TrackCount { total: 50 }),
            ..Playlist::default()
        }))]);
        let entry = app.engine.library_entry("blank").unwrap();
        assert_eq!(entry.name, "This Is TUYU");
        assert_eq!(entry.images, vec![cover.clone()]);
        assert_eq!(entry.description.as_deref(), Some("The essential tracks."));
        assert_eq!(entry.track_total(), 50);
        assert_eq!(
            app.engine.library_entry("later").unwrap().name,
            "",
            "a row fills when its own header arrives, not before"
        );
        assert_eq!(app.engine.library_entry("named").unwrap().name, "Mix");

        app.handle_backend_events(vec![Event::PlaylistHeader(Box::new(Playlist {
            id: "later".into(),
            name: "This Is YOASOBI".into(),
            ..Playlist::default()
        }))]);
        assert_eq!(app.engine.library_entry("later").unwrap().name, "This Is YOASOBI");

        app.engine.load_playlists();
        app.engine.playlist_headers_requested.clear();
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: listed(),
        });
        assert_eq!(app.engine.library_entry("blank").unwrap().name, "This Is TUYU");
        assert_eq!(app.engine.library_entry("later").unwrap().name, "This Is YOASOBI");
        assert!(
            app.engine.playlist_headers_requested.is_empty(),
            "a header already read is not asked for again"
        );
    }

    /// Following, unfollowing or editing a playlist reads the library's
    /// playlists again from the top, while the list shown stays. A later
    /// page asked for before that belongs to the old list: taking it made
    /// that page the whole list, and the pages it led on to ran beside the
    /// new ones and repeated them.
    #[test]
    fn a_playlist_page_asked_for_before_a_reload_is_not_taken() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);

        app.engine.load_playlists();
        let old = app.engine.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: old,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        // The second page is on its way when following a playlist reloads.
        app.handle_api(ApiResponse::PlaylistFollowChanged {
            id: "new".into(),
            followed: true,
            result: Ok(()),
        });
        let shown = playlist_ids(&["a", "b"]);
        assert_eq!(listed_playlists(&app), shown, "the list shown stays");
        let new = app.engine.library.playlists_generation;
        assert_ne!(new, old, "a reload is a new load");
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: old,
            result: Ok(playlist_page(&["c"], 2, 3)),
        });
        assert_eq!(
            listed_playlists(&app),
            shown,
            "the late page is not the reloaded list"
        );
        assert_eq!(app.engine.library.playlists_next, None, "and asks for nothing");

        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: new,
            result: Ok(playlist_page(&["new", "a"], 0, 4)),
        });
        assert_eq!(
            listed_playlists(&app),
            shown,
            "the reloaded list replaces it only once whole"
        );
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: new,
            result: Ok(playlist_page(&["b", "c"], 2, 4)),
        });
        let whole = playlist_ids(&["new", "a", "b", "c"]);
        assert_eq!(listed_playlists(&app), whole);
        // Another answer for a page already taken adds nothing.
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: new,
            result: Ok(playlist_page(&["b", "c"], 2, 4)),
        });
        assert_eq!(listed_playlists(&app), whole);
    }

    /// An offset does not say which load a page belongs to. Once the
    /// reloaded list has asked for its own second page, the old load's
    /// second page, arriving late at the same offset, is still not taken:
    /// taking it ended the list early and dropped the real second page.
    #[test]
    fn an_old_playlist_page_at_the_offset_the_reload_asked_for_is_not_taken() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);

        app.engine.load_playlists();
        let old = app.engine.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: old,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        assert_eq!(app.engine.library.playlists_asked, Some(2));
        app.handle_api(ApiResponse::PlaylistFollowChanged {
            id: "new".into(),
            followed: true,
            result: Ok(()),
        });
        let new = app.engine.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: new,
            result: Ok(playlist_page(&["new", "a"], 0, 4)),
        });
        assert_eq!(
            app.engine.library.playlists_asked,
            Some(2),
            "the reloaded list asks for the same offset"
        );

        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: old,
            result: Ok(playlist_page(&["c"], 2, 3)),
        });
        assert_eq!(
            listed_playlists(&app),
            playlist_ids(&["a", "b"]),
            "the old load's page is not taken, and the list shown stays"
        );
        assert_eq!(
            app.engine.library.playlists_asked,
            Some(2),
            "the reloaded list still waits for its own page"
        );

        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: new,
            result: Ok(playlist_page(&["b", "c"], 2, 4)),
        });
        assert_eq!(
            listed_playlists(&app),
            playlist_ids(&["new", "a", "b", "c"])
        );
    }

    /// A later page that failed is no longer on its way, so another answer
    /// for it is not taken into the list.
    #[test]
    fn a_failed_playlist_page_is_no_longer_awaited() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);

        app.engine.load_playlists();
        let generation = app.engine.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(app.engine.library.playlists_asked, None);
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation,
            result: Ok(playlist_page(&["c"], 2, 3)),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["a", "b"]));
    }

    /// Signing out forgets the library, but not which load came last: a
    /// page the old account asked for does not become the next account's
    /// list.
    #[test]
    fn a_playlist_page_asked_for_before_sign_out_is_not_taken() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);

        app.engine.load_playlists();
        let old = app.engine.library.playlists_generation;
        app.engine.reset_data();
        app.engine.load_playlists();
        assert_ne!(app.engine.library.playlists_generation, old);
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: old,
            result: Ok(playlist_page(&["theirs"], 0, 1)),
        });
        assert!(app.engine.library.playlists.is_loading());
    }

    fn listener_app() -> App {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.engine.account_caches_generation = 7;
        app
    }

    fn cached_playlists(ids: &[&str]) -> Option<Vec<Playlist>> {
        Some(playlist_page(ids, 0, ids.len() as u32).items)
    }

    fn stored_playlists(app: &App) -> Vec<(String, Vec<String>)> {
        app.engine.backend
            .take_library_cache_stores()
            .into_iter()
            .map(|cache| {
                let ids = cache.data.into_iter().map(|playlist| playlist.id).collect();
                (cache.account_id, ids)
            })
            .collect()
    }

    /// The last list Spotify gave shows the moment the account is
    /// verified, and only for that account. The reload behind it replaces
    /// it once whole, and that list is what the next start shows.
    #[test]
    fn the_cached_playlist_list_shows_until_its_reload_is_whole() {
        let mut app = listener_app();
        app.engine.load_playlists();
        app.engine.receive_account_caches("bob", 7, cached_playlists(&["theirs"]), None);
        app.engine.receive_account_caches("alice", 6, cached_playlists(&["earlier"]), None);
        assert!(app.engine.library.playlists.is_loading(), "neither is this list");

        app.engine.receive_account_caches("alice", 7, cached_playlists(&["x", "y"]), None);
        assert_eq!(listed_playlists(&app), playlist_ids(&["x", "y"]));
        assert_eq!(app.engine.saved.get("spotify:playlist:x"), Some(&true));

        let generation = app.engine.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["x", "y"]));
        assert!(stored_playlists(&app).is_empty(), "nothing whole to save");
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation,
            result: Ok(playlist_page(&["c"], 2, 3)),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["a", "b", "c"]));
        assert_eq!(app.engine.library.playlists_loading_since, None);
        assert_eq!(
            stored_playlists(&app),
            vec![(
                "alice".to_string(),
                vec!["a".into(), "b".into(), "c".into()]
            )]
        );
    }

    /// A list Spotify already answered with is newer than any cache.
    #[test]
    fn a_late_cache_does_not_replace_the_live_playlist_list() {
        let mut app = listener_app();
        app.engine.load_playlists();
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(playlist_page(&["a"], 0, 1)),
        });
        app.engine.receive_account_caches("alice", 7, cached_playlists(&["x"]), None);
        assert_eq!(listed_playlists(&app), playlist_ids(&["a"]));
    }

    /// Spotify turning a reload away leaves the list shown and asks again
    /// later; with nothing to show, the error shows and is retried too. An
    /// answer that will not change by waiting is not asked again.
    #[test]
    fn a_turned_away_playlist_load_keeps_the_list_and_asks_again() {
        let mut app = listener_app();
        app.engine.load_playlists();
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert!(matches!(app.engine.library.playlists, Loadable::Failed(_)));
        assert!(app.engine.library.playlists_retry_at.is_some());

        app.engine.receive_account_caches("alice", 7, cached_playlists(&["x"]), None);
        assert_eq!(listed_playlists(&app), playlist_ids(&["x"]));
        app.engine.load_playlists();
        assert_eq!(app.engine.library.playlists_retry_at, None);
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Err(crate::api::ApiError::Network("offline".into())),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["x"]));
        assert!(app.engine.library.playlists_retry_at.is_some());
        assert_eq!(app.engine.library.playlists_loading_since, None);

        app.engine.load_playlists();
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Err(crate::api::ApiError::Status {
                status: 403,
                message: "forbidden".into(),
            }),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["x"]));
        assert_eq!(app.engine.library.playlists_retry_at, None);
    }

    /// A playlist created or deleted while a reload is on its way stays
    /// that way when the reload, asked for before, answers without it.
    /// Once a load asked for after Spotify confirmed it answers, the change
    /// is Spotify's own and the list is saved.
    #[test]
    fn playlists_created_or_deleted_during_a_reload_do_not_come_back() {
        let mut app = listener_app();
        let ctx = egui::Context::default();
        app.engine.receive_account_caches("alice", 7, cached_playlists(&["a", "b"]), None);
        app.engine.load_playlists();
        let before = app.engine.library.playlists_generation;

        app.apply(Action::DeletePlaylist("a".into()), &ctx);
        app.handle_api(ApiResponse::PlaylistCreated(Ok(Playlist {
            id: "n".into(),
            uri: "spotify:playlist:n".into(),
            ..Playlist::default()
        })));
        assert_eq!(listed_playlists(&app), playlist_ids(&["n", "b"]));

        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: before,
            result: Ok(playlist_page(&["a", "b"], 0, 2)),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["n", "b"]));
        assert_eq!(app.engine.saved.get("spotify:playlist:a"), Some(&false));
        assert!(
            stored_playlists(&app).is_empty(),
            "an unconfirmed deletion is not saved"
        );

        app.handle_api(ApiResponse::PlaylistFollowChanged {
            id: "a".into(),
            followed: false,
            result: Ok(()),
        });
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(playlist_page(&["n", "b"], 0, 2)),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["n", "b"]));
        assert!(app.engine.library.playlists_edits.is_empty());
        assert_eq!(
            stored_playlists(&app),
            vec![("alice".to_string(), vec!["n".into(), "b".into()])]
        );
    }

    /// Offline, a list Spotify could not be reached for is not asked for
    /// again each minute; the network coming back asks at once, and the
    /// list shown stays meanwhile.
    #[test]
    fn a_turned_away_playlist_list_is_asked_again_when_the_network_is_back() {
        let mut app = listener_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.receive_account_caches("alice", 7, cached_playlists(&["x"]), None);
        app.engine.load_playlists();
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Err(crate::api::ApiError::Network("offline".into())),
        });
        assert!(app.engine.library.playlists_retry_at.is_some());

        app.handle_backend_events(vec![Event::NetworkStatus(false)]);
        let offline = app.engine.library.playlists_generation;
        app.engine.load_playlists();
        assert_eq!(
            app.engine.library.playlists_generation, offline,
            "nothing is asked for offline"
        );

        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        assert_ne!(app.engine.library.playlists_generation, offline);
        assert!(app.engine.library.playlists_loading_since.is_some());
        assert_eq!(listed_playlists(&app), playlist_ids(&["x"]));
    }

    /// A deletion Spotify refused is forgotten: the next list shows the
    /// playlist again.
    #[test]
    fn a_refused_playlist_deletion_lists_the_playlist_again() {
        let mut app = listener_app();
        let ctx = egui::Context::default();
        app.engine.receive_account_caches("alice", 7, cached_playlists(&["a", "b"]), None);
        app.apply(Action::DeletePlaylist("a".into()), &ctx);
        app.handle_api(ApiResponse::PlaylistFollowChanged {
            id: "a".into(),
            followed: false,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert!(app.engine.library.playlists_edits.is_empty());
        app.engine.load_playlists();
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(playlist_page(&["a", "b"], 0, 2)),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["a", "b"]));
    }

    fn played(uri: &str) -> crate::api::models::PlayHistory {
        crate::api::models::PlayHistory {
            track: song(uri),
            played_at: None,
            context: None,
        }
    }

    fn song(uri: &str) -> Track {
        Track {
            id: Some(uri.into()),
            uri: uri.into(),
            ..Track::default()
        }
    }

    fn cached_home() -> HomeSnapshot {
        HomeSnapshot {
            made_for_you: None,
            made_for_you_personalized: false,
            recently_played: Some(vec![played("spotify:track:cached-recent")]),
            top_artists: Some(vec![Artist {
                id: "cached-artist".into(),
                ..Artist::default()
            }]),
            top_tracks: Some(vec![song("spotify:track:cached-top")]),
            discover: [(
                "Discover Weekly".to_string(),
                cached_playlists(&["weekly"]).unwrap(),
            )]
            .into(),
            podcasts: vec![(
                Show {
                    id: "show".into(),
                    ..Show::default()
                },
                Vec::new(),
            )],
        }
    }

    /// The last Home shows the moment the account is verified. Each shelf
    /// Spotify answers for replaces its cached one and is saved; one it
    /// turns away keeps it.
    #[test]
    fn the_cached_home_shows_until_spotify_answers_for_each_shelf() {
        let mut app = listener_app();
        app.engine.load_home(false);
        let generation = app.engine.home.generation;
        app.engine.receive_account_caches("alice", 7, None, Some(cached_home()));
        assert_eq!(
            app.engine.home.recently_played.get(),
            Some(&vec![played("spotify:track:cached-recent")])
        );
        assert!(app.engine.home.made_for_you.get().is_some());
        assert_eq!(app.engine.home.podcasts.len(), 1);

        app.handle_api(ApiResponse::RecentlyPlayed {
            who: RecentsFor::Home,
            generation,
            limit: HOME_RECENTS,
            result: Ok(crate::api::models::CursorPage {
                items: vec![played("spotify:track:fresh")],
                ..Default::default()
            }),
        });
        app.handle_api(ApiResponse::TopArtists {
            generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        app.handle_api(ApiResponse::TopTracks {
            offset: 0,
            full: false,
            generation,
            result: Ok(crate::api::models::Page {
                items: vec![song("spotify:track:fresh-top")],
                ..Default::default()
            }),
        });
        app.handle_api(ApiResponse::MadeForYou {
            generation: app.engine.home.made_for_you_generation,
            personalized: false,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(
            app.engine.home.recently_played.get(),
            Some(&vec![played("spotify:track:fresh")])
        );
        assert_eq!(app.engine.home.top_artists.get().map(Vec::len), Some(1));
        assert!(
            app.engine.home.made_for_you.get().is_some(),
            "a turned away search keeps what it showed"
        );

        // The failed shelf refresh keeps its cached cards. Successful
        // recently played and top tracks answers are saved independently.
        let stored = app.engine.backend.take_home_cache_stores();
        assert_eq!(stored.len(), 2);
        assert_eq!(app.engine.home.awaiting, 0);
        let saved = stored.last().unwrap();
        assert_eq!(saved.account_id, "alice");
        assert_eq!(
            saved.data.recently_played,
            Some(vec![played("spotify:track:fresh")])
        );
        assert_eq!(saved.data.top_artists, cached_home().top_artists);
        assert_eq!(
            saved.data.top_tracks,
            Some(vec![song("spotify:track:fresh-top")])
        );
        assert_eq!(saved.data.made_for_you.as_ref().map(Vec::len), Some(1));
        assert_eq!(saved.data.podcasts.len(), 1);
    }

    /// A shelf Spotify has not answered for is saved as unanswered, not as
    /// empty, so the next start shows it loading, and later its error,
    /// rather than an empty shelf. Older files with every shelf read back.
    #[test]
    fn an_unanswered_home_shelf_is_not_saved_as_empty() {
        let mut app = listener_app();
        app.engine.load_home(false);
        app.handle_api(ApiResponse::RecentlyPlayed {
            who: RecentsFor::Home,
            generation: app.engine.home.generation,
            limit: HOME_RECENTS,
            result: Ok(crate::api::models::CursorPage {
                items: vec![played("spotify:track:fresh")],
                ..Default::default()
            }),
        });
        let saved = app.engine.backend.take_home_cache_stores().pop().unwrap().data;
        assert!(saved.recently_played.is_some());
        assert_eq!(saved.top_artists, None);
        assert_eq!(saved.top_tracks, None);

        let mut next = listener_app();
        next.engine.load_home(false);
        next.engine.receive_account_caches("alice", 7, None, Some(saved));
        assert!(next.engine.home.recently_played.get().is_some());
        assert!(next.engine.home.top_artists.is_loading());
        assert!(next.engine.home.top_tracks.is_loading());

        let older: HomeSnapshot = serde_json::from_str(
            r#"{"recently_played":[],"top_artists":[],"top_tracks":[],"discover":{},"podcasts":[]}"#,
        )
        .unwrap();
        assert_eq!(older.top_artists, Some(Vec::new()));
    }

    /// Shelves Spotify already answered for are newer than the cache.
    #[test]
    fn a_late_home_cache_does_not_replace_live_shelves() {
        let mut app = listener_app();
        app.engine.load_home(false);
        app.handle_api(ApiResponse::RecentlyPlayed {
            who: RecentsFor::Home,
            generation: app.engine.home.generation,
            limit: HOME_RECENTS,
            result: Ok(crate::api::models::CursorPage {
                items: vec![played("spotify:track:fresh")],
                ..Default::default()
            }),
        });
        app.engine.receive_account_caches("alice", 7, None, Some(cached_home()));
        assert_eq!(
            app.engine.home.recently_played.get(),
            Some(&vec![played("spotify:track:fresh")])
        );
        assert_eq!(
            app.engine.home.top_tracks.get(),
            Some(&vec![song("spotify:track:cached-top")])
        );
    }

    fn connected_listener() -> App {
        let mut app = listener_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app
    }

    /// The network dropping between two pages ends the load with the list
    /// shown, so the spinner stops, and its return reads the list again.
    #[test]
    fn a_playlist_load_cut_off_between_pages_ends_and_resumes_with_the_network() {
        let mut app = connected_listener();
        app.engine.receive_account_caches("alice", 7, cached_playlists(&["x"]), None);
        app.engine.load_playlists();
        app.handle_backend_events(vec![Event::NetworkStatus(false)]);
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["x"]));
        assert_eq!(app.engine.library.playlists_loading_since, None);
        assert_eq!(app.engine.library.playlists_incoming, None);
        assert_ne!(app.background_activity(), Some(BackgroundActivity::Library));

        let cut_off = app.engine.library.playlists_generation;
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        assert_ne!(app.engine.library.playlists_generation, cut_off);
        assert!(app.engine.library.playlists_loading_since.is_some());
    }

    /// With no list shown yet, a load cut off between pages keeps the pages
    /// that arrived, is not saved as the account's list, and is read again
    /// once the network is back.
    #[test]
    fn a_first_load_cut_off_between_pages_keeps_its_pages_and_resumes() {
        let mut app = connected_listener();
        app.engine.load_playlists();
        app.handle_backend_events(vec![Event::NetworkStatus(false)]);
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["a", "b"]));
        assert!(!app.engine.library.playlists_whole);
        assert_eq!(app.engine.library.playlists_loading_since, None);
        assert!(app.engine.library.playlists_retry_at.is_some());
        assert!(stored_playlists(&app).is_empty());

        let cut_off = app.engine.library.playlists_generation;
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        assert_ne!(app.engine.library.playlists_generation, cut_off);
        assert_eq!(
            listed_playlists(&app),
            playlist_ids(&["a", "b"]),
            "the pages shown stay while the list is read again"
        );
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(playlist_page(&["a", "b", "c"], 0, 3)),
        });
        assert!(app.engine.library.playlists_whole);
        assert_eq!(
            stored_playlists(&app),
            vec![(
                "alice".to_string(),
                vec!["a".into(), "b".into(), "c".into()]
            )]
        );
    }

    /// Opening Home after a refresh failed behind cached shelves reads it
    /// again at once, rather than after ten minutes.
    #[test]
    fn opening_home_after_a_failed_refresh_reads_it_again() {
        let mut app = connected_listener();
        app.engine.load_home(false);
        let generation = app.engine.home.generation;
        app.handle_api(ApiResponse::TopArtists {
            generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        app.engine.ensure_loaded(Page::Home);
        assert_ne!(app.engine.home.generation, generation);
        let again = app.engine.home.generation;
        app.engine.ensure_loaded(Page::Home);
        assert_eq!(
            app.engine.home.generation, again,
            "a fresh refresh is not repeated"
        );
    }

    /// Starting offline, the cached list shows and is read again once the
    /// network is back.
    #[test]
    fn a_cached_list_restored_offline_is_read_again_with_the_network() {
        let mut app = connected_listener();
        app.handle_backend_events(vec![Event::NetworkStatus(false)]);
        let offline = app.engine.library.playlists_generation;
        app.engine.load_playlists();
        app.engine.receive_account_caches("alice", 7, cached_playlists(&["x"]), None);
        assert_eq!(listed_playlists(&app), playlist_ids(&["x"]));
        assert_eq!(app.engine.library.playlists_generation, offline);

        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        assert_ne!(app.engine.library.playlists_generation, offline);
    }

    /// A list that stopped short of its last page is never saved as the
    /// account's list, even when a playlist created afterwards would save
    /// a whole one.
    #[test]
    fn a_partial_playlist_list_is_not_saved() {
        let mut app = connected_listener();
        app.engine.load_playlists();
        let generation = app.engine.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation,
            result: Err(crate::api::ApiError::Status {
                status: 403,
                message: "forbidden".into(),
            }),
        });
        app.handle_api(ApiResponse::PlaylistCreated(Ok(Playlist {
            id: "n".into(),
            uri: "spotify:playlist:n".into(),
            ..Playlist::default()
        })));
        assert_eq!(listed_playlists(&app), playlist_ids(&["n", "a", "b"]));
        assert!(stored_playlists(&app).is_empty());
    }

    /// A Home refresh that failed behind cached shelves is read again when
    /// the network comes back, though every shelf shows rows.
    #[test]
    fn a_failed_home_refresh_behind_cached_shelves_is_read_again() {
        let mut app = connected_listener();
        app.engine.load_home(false);
        let generation = app.engine.home.generation;
        app.engine.receive_account_caches("alice", 7, None, Some(cached_home()));
        app.handle_api(ApiResponse::TopArtists {
            generation,
            result: Err(crate::api::ApiError::Network("offline".into())),
        });
        assert!(
            app.engine.home.top_artists.get().is_some(),
            "the cached shelf stays"
        );

        app.handle_backend_events(vec![Event::NetworkStatus(false)]);
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        assert_ne!(app.engine.home.generation, generation);
        assert!(!app.engine.home.refresh_failed);
    }

    /// Podcasts Spotify answered for, even with none, are not replaced by a
    /// cache that arrives late.
    #[test]
    fn a_late_cache_does_not_bring_back_answered_podcasts() {
        let mut app = connected_listener();
        app.engine.load_home(false);
        app.handle_api(ApiResponse::HomeEpisodes {
            generation: app.engine.home.generation,
            result: Ok(Vec::new()),
        });
        app.engine.receive_account_caches("alice", 7, None, Some(cached_home()));
        assert!(app.engine.home.podcasts.is_empty());
    }

    /// A session connecting reads its account's cache once `Me` names the
    /// account, the same one again or another, and only then: a read asked
    /// for the account before it would only be thrown away.
    #[test]
    fn connecting_reads_the_cache_once_for_the_account_me_names() {
        for (account, why) in [("alice", "the same account"), ("bob", "another account")] {
            let mut app = connected_listener();
            app.engine.handle_auth(AuthStatus::Connected {
                username: account.into(),
            });
            assert_eq!(app.engine.account_caches_generation, 7, "{why}: read before Me");
            app.handle_api(ApiResponse::Me(Ok(User {
                id: account.into(),
                ..Default::default()
            })));
            let generation = app.engine.account_caches_generation;
            assert_ne!(generation, 7, "{why}: not read");
            app.handle_api(ApiResponse::Me(Ok(User {
                id: account.into(),
                ..Default::default()
            })));
            assert_eq!(
                app.engine.account_caches_generation, generation,
                "{why}: read again on the next Me"
            );
            app.engine.receive_account_caches(account, generation, cached_playlists(&["x"]), None);
            assert_eq!(listed_playlists(&app), playlist_ids(&["x"]), "{why}");
        }
    }

    /// Signing out forgets Home, but not which refresh came last: an answer
    /// the old account asked for does not fill the next account's Home.
    #[test]
    fn a_home_answer_asked_for_before_sign_out_is_not_taken() {
        let mut app = listener_app();
        app.engine.load_home(false);
        let old = app.engine.home.generation;
        app.engine.reset_data();
        app.engine.load_home(false);
        assert_ne!(app.engine.home.generation, old);
        app.handle_api(ApiResponse::TopArtists {
            generation: old,
            result: Ok(vec![Artist::default()]),
        });
        assert!(app.engine.home.top_artists.is_loading());
    }

    /// The top bar's spinner names the background work that has gone on
    /// for a moment, the library before an update check, and none of it
    /// while the app is offline.
    #[test]
    fn the_spinner_names_background_work_that_outlasts_a_moment() {
        let mut app = listener_app();
        let a_while_ago = || Some(Instant::now() - BACKGROUND_QUIET);
        assert_eq!(app.background_activity(), None);

        app.update_checking = true;
        app.last_update_check = Some(Instant::now());
        assert_eq!(app.background_activity(), None, "a fast check never shows");
        app.last_update_check = a_while_ago();
        assert_eq!(app.background_activity(), Some(BackgroundActivity::Updates));

        app.engine.load_playlists();
        assert_eq!(
            app.background_activity(),
            Some(BackgroundActivity::Updates),
            "a reload that has just begun does not show yet"
        );
        app.engine.library.playlists_loading_since = a_while_ago();
        assert_eq!(app.background_activity(), Some(BackgroundActivity::Library));
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(playlist_page(&["a"], 0, 1)),
        });
        assert_eq!(
            app.background_activity(),
            Some(BackgroundActivity::Updates),
            "a whole list ends the library's work"
        );

        app.engine.load_home(false);
        app.engine.home.loaded_at = a_while_ago();
        assert_eq!(app.background_activity(), Some(BackgroundActivity::Library));
        app.engine.offline = true;
        assert_eq!(app.background_activity(), None);
    }

    /// Signing out removes the account's caches from disk; a read of them
    /// that answers after it shows nothing.
    #[test]
    fn account_caches_read_before_sign_out_show_nothing_after_it() {
        let mut app = listener_app();
        app.engine.handle_auth(AuthStatus::SignedOut);
        app.engine.receive_account_caches("alice", 7, cached_playlists(&["a"]), Some(cached_home()));
        assert!(app.engine.library.playlists.get().is_none());
        assert!(app.engine.home.recently_played.get().is_none());
        assert!(app.engine.home.made_for_you.get().is_none());
    }

    /// The streaming session does not always name a playlist's owner. The
    /// account's own name stands in for its own lists, the library list's
    /// for the rest it holds, in whichever order the answers arrive, and
    /// a name Spotify gave stays.
    #[test]
    fn a_header_without_an_owner_name_takes_a_known_one() {
        use crate::api::models::{Owner, User};
        let owned_by = |id: &str, name: Option<&str>| Owner {
            id: Some(id.into()),
            display_name: name.map(str::to_string),
            uri: None,
        };
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "me".into(),
            display_name: Some("Mine".into()),
            ..User::default()
        });
        app.engine.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl2".into(),
            owner: owned_by("other", Some("Molly C.")),
            ..Playlist::default()
        }]);
        for id in ["pl1", "pl2", "pl3"] {
            app.engine.playlist_pages.insert(
                id.into(),
                PlaylistPage {
                    generation: 1,
                    ..Default::default()
                },
            );
        }
        let header = |id: &str, owner: Owner| ApiResponse::Playlist {
            id: id.into(),
            generation: 1,
            result: Ok(Playlist {
                id: id.into(),
                owner,
                ..Playlist::default()
            }),
        };
        let shown = |app: &App, id: &str| {
            app.engine.playlist_pages[id]
                .playlist
                .get()
                .unwrap()
                .owner_name()
                .to_string()
        };
        app.handle_api(header("pl1", owned_by("me", None)));
        app.handle_api(header("pl2", owned_by("other", None)));
        app.handle_api(header("pl3", owned_by("nobody", None)));
        assert_eq!(shown(&app, "pl1"), "Mine", "the account's own name");
        assert_eq!(shown(&app, "pl2"), "Molly C.", "the library list's");
        assert_eq!(
            shown(&app, "pl3"),
            "nobody",
            "the id until someone names them"
        );

        // The list can arrive after the header: the page takes the name
        // then, and a name Spotify already gave stays.
        app.handle_api(header("pl2", owned_by("other", Some("Molly"))));
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![
                    Playlist {
                        id: "pl2".into(),
                        owner: owned_by("other", Some("Molly C.")),
                        ..Playlist::default()
                    },
                    Playlist {
                        id: "pl3".into(),
                        owner: owned_by("nobody", Some("Nobody")),
                        ..Playlist::default()
                    },
                ],
                ..Default::default()
            }),
        });
        assert_eq!(shown(&app, "pl3"), "Nobody");
        assert_eq!(shown(&app, "pl2"), "Molly");
    }

    /// Opening a playlist asks about its owner once, so the profile view's
    /// picture can stand beside the name; a header without an owner asks
    /// about nobody, and the answers land as a name and a picture.
    #[test]
    fn a_playlist_page_asks_about_its_owner() {
        use crate::api::models::Owner;
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "pl1".into(),
            PlaylistPage {
                generation: 1,
                ..Default::default()
            },
        );
        let header = |owner: Option<&str>| ApiResponse::Playlist {
            id: "pl1".into(),
            generation: 1,
            result: Ok(Playlist {
                id: "pl1".into(),
                owner: Owner {
                    id: owner.map(str::to_string),
                    ..Default::default()
                },
                ..Playlist::default()
            }),
        };
        app.handle_api(header(Some("alice")));
        assert!(
            app.engine.user_names.contains_key("alice"),
            "the owner was asked about"
        );
        app.handle_api(header(None));
        assert!(
            !app.engine.user_names.contains_key(""),
            "nobody to ask without an owner"
        );
        app.engine.set_user_name("alice".into(), Some("Alice".into()));
        app.engine.set_user_image("alice".into(), Some("https://i.scdn.co/image/a".into()));
        assert_eq!(
            app.engine.user_names.get("alice").and_then(Option::as_deref),
            Some("Alice")
        );
        assert_eq!(
            app.engine.user_images.get("alice").and_then(Option::as_deref),
            Some("https://i.scdn.co/image/a")
        );
    }

    /// The streaming session carries no cover for a playlist without one
    /// of its own, where the Web API composes a mosaic; the library list
    /// holds that mosaic, in whichever order the answers arrive.
    #[test]
    fn a_header_without_a_cover_takes_the_library_lists() {
        let cover = |url: &str| {
            vec![Image {
                url: url.into(),
                width: Some(640),
                height: Some(640),
            }]
        };
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            images: cover("https://mosaic.scdn.co/640/pl1"),
            ..Playlist::default()
        }]);
        for id in ["pl1", "pl2", "pl3"] {
            app.engine.playlist_pages.insert(
                id.into(),
                PlaylistPage {
                    generation: 1,
                    ..Default::default()
                },
            );
        }
        let header = |id: &str, images: Vec<Image>| ApiResponse::Playlist {
            id: id.into(),
            generation: 1,
            result: Ok(Playlist {
                id: id.into(),
                images,
                ..Playlist::default()
            }),
        };
        let shown = |app: &App, id: &str| {
            app.engine.playlist_pages[id]
                .playlist
                .get()
                .unwrap()
                .images
                .iter()
                .map(|image| image.url.clone())
                .collect::<Vec<_>>()
        };
        app.handle_api(header("pl1", Vec::new()));
        app.handle_api(header("pl2", cover("https://i.scdn.co/image/own")));
        app.handle_api(header("pl3", Vec::new()));
        assert_eq!(
            shown(&app, "pl1"),
            ["https://mosaic.scdn.co/640/pl1"],
            "the library list's mosaic"
        );
        assert_eq!(
            shown(&app, "pl2"),
            ["https://i.scdn.co/image/own"],
            "a cover of its own stays"
        );
        assert!(shown(&app, "pl3").is_empty(), "nothing to take it from");

        // The list can arrive after the header: the page takes the cover
        // then, and one the header carried stays.
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![
                    Playlist {
                        id: "pl2".into(),
                        images: cover("https://mosaic.scdn.co/640/pl2"),
                        ..Playlist::default()
                    },
                    Playlist {
                        id: "pl3".into(),
                        images: cover("https://mosaic.scdn.co/640/pl3"),
                        ..Playlist::default()
                    },
                ],
                ..Default::default()
            }),
        });
        assert_eq!(shown(&app, "pl3"), ["https://mosaic.scdn.co/640/pl3"]);
        assert_eq!(shown(&app, "pl2"), ["https://i.scdn.co/image/own"]);
    }

    #[test]
    fn volume_conversions_round_trip() {
        assert_eq!(volume_to_percent(u16::MAX), 100);
        assert_eq!(volume_to_percent(0), 0);
        assert_eq!(volume_to_percent(percent_to_volume(70)), 70);
        assert_eq!(percent_to_volume(200), u16::MAX);
    }

    /// Toggling always-on-top pushes the matching level to the live Winamp
    /// window, so the change takes effect without recreating the window.
    #[test]
    fn the_winamp_window_follows_the_on_top_toggle_live() {
        assert_eq!(
            winamp_on_top_level(true, true),
            Some(egui::WindowLevel::AlwaysOnTop)
        );
        assert_eq!(
            winamp_on_top_level(true, false),
            Some(egui::WindowLevel::Normal)
        );
    }

    /// The setting lives in the big window's Settings page. Toggling it there
    /// must never force the big window on top, so no level command is sent.
    #[test]
    fn the_big_window_never_follows_the_on_top_toggle() {
        assert_eq!(winamp_on_top_level(false, true), None);
        assert_eq!(winamp_on_top_level(false, false), None);
    }

    /// The level set at window creation does not stick on X11, so opening the
    /// Winamp window with always-on-top saved must schedule a re-assert.
    #[test]
    fn opening_the_winamp_window_on_top_schedules_a_reassert() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.settings.winamp_window = true;
        app.engine.settings.winamp_on_top = true;
        app.attach(&ctx);
        assert_eq!(app.winamp_level_reassert, 3);
    }

    /// Opening the Winamp window without always-on-top re-asserts nothing.
    #[test]
    fn opening_the_winamp_window_without_on_top_schedules_no_reassert() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.settings.winamp_window = true;
        app.engine.settings.winamp_on_top = false;
        app.attach(&ctx);
        assert_eq!(app.winamp_level_reassert, 0);
    }

    #[test]
    fn unsupported_on_top_controls_keep_the_saved_preference_without_commands() {
        for saved in [false, true] {
            let ctx = egui::Context::default();
            let mut app = headless_app();
            app.engine.settings.winamp_window = true;
            app.engine.settings.winamp_on_top = saved;
            app.window_level_supported = false;
            app.attach(&ctx);
            assert_eq!(app.winamp_level_reassert, 0);
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.apply(Action::ToggleWinampOnTop, ui.ctx());
                app.push_winamp_level(ui.ctx());
            });
            output.textures_delta.clear();
            assert_eq!(app.engine.settings.winamp_on_top, saved);
            assert!(!app.engine.settings_dirty);
            assert!(
                !output.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .iter()
                    .any(|command| matches!(command, egui::ViewportCommand::WindowLevel(_)))
            );
            app.engine.backend.shutdown();
        }
    }

    #[test]
    fn returning_to_the_mini_player_restores_its_position_and_shade() {
        let mut app = headless_app();
        app.engine.settings.winamp_window = true;
        app.engine.settings.winamp_shaded = true;
        app.winamp.geometry.position = Some([300.0, 200.0]);
        let main = fastframe_shell::window::Geometry {
            size: Some([1024.0, 768.0]),
            position: Some([100.0, 100.0]),
            maximized: false,
        };
        app.window_geometry = main;

        let main_ctx = egui::Context::default();
        app.apply(Action::ToggleWinampWindow, &main_ctx);
        app.attach(&main_ctx);
        app.apply(Action::ToggleWinampWindow, &main_ctx);

        let mini_ctx = egui::Context::default();
        let mut output = mini_ctx.run_ui(Default::default(), |_ui| app.attach(&mini_ctx));
        output.textures_delta.clear();
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(app.engine.settings.winamp_window);
        assert!(app.engine.settings.winamp_shaded);
        assert!(commands.contains(&egui::ViewportCommand::Fullscreen(false)));
        assert!(commands.contains(&egui::ViewportCommand::Maximized(false)));
        // The position goes to the window's builder before it exists
        // (entrypoint's native_options), not after it in a command.
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::OuterPosition(_)))
        );
        assert_eq!(app.winamp.geometry.position, Some([300.0, 200.0]));
        assert_eq!(app.window_geometry, main);
    }

    /// A window left maximized or full screen comes back that way, instead of
    /// being restored down by the session's size and position.
    #[test]
    fn a_window_that_fills_the_screen_keeps_its_state_over_the_session_geometry() {
        // On Windows only fullscreen lyrics make the frameless main window
        // full screen, so a restored full screen is left instead; see
        // a_window_closed_in_fullscreen_lyrics_returns_to_its_previous_mode.
        let cases: &[(&str, Option<bool>, Option<bool>)] = if cfg!(windows) {
            &[("maximized", Some(true), None)]
        } else {
            &[
                ("maximized", Some(true), None),
                ("full screen", None, Some(true)),
            ]
        };
        for &(name, maximized, fullscreen) in cases {
            let mut app = headless_app();
            app.window_geometry.size = Some([1024.0, 768.0]);
            app.window_geometry.position = Some([100.0, 150.0]);

            let ctx = egui::Context::default();
            let mut raw_input = egui::RawInput::default();
            let viewport = raw_input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default();
            viewport.maximized = maximized;
            viewport.fullscreen = fullscreen;

            let mut output = ctx.run_ui(raw_input, |_ui| app.attach(&ctx));
            output.textures_delta.clear();
            let commands = &output
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .expect("the root viewport")
                .commands;
            assert!(
                !commands.iter().any(|command| matches!(
                    command,
                    egui::ViewportCommand::InnerSize(_) | egui::ViewportCommand::OuterPosition(_)
                )),
                "a {name} window is neither resized nor moved: {commands:?}"
            );
            app.engine.backend.shutdown();
        }
    }

    /// Closing the app in fullscreen lyrics leaves eframe to restore the
    /// window full screen without them, and on Windows nothing else could
    /// leave it. The next start returns it to the mode the lyrics came from.
    #[test]
    fn a_window_closed_in_fullscreen_lyrics_returns_to_its_previous_mode() {
        use crate::settings::WindowMode;
        use egui::ViewportCommand::{Fullscreen, InnerSize, Maximized};
        let ordinary = WindowMode::default();
        let maximized = WindowMode {
            maximized: true,
            ..WindowMode::default()
        };
        let full_screen = WindowMode {
            fullscreen: true,
            ..WindowMode::default()
        };
        for (left, expected) in [
            (Some(ordinary), vec![Fullscreen(false)]),
            (Some(maximized), vec![Fullscreen(false), Maximized(true)]),
            (Some(full_screen), vec![Fullscreen(true)]),
            // A session from before this was remembered: Windows can only
            // have been in fullscreen lyrics, other desktops keep the state.
            (
                None,
                if cfg!(windows) {
                    vec![Fullscreen(false)]
                } else {
                    vec![]
                },
            ),
        ] {
            let mut app = headless_app();
            app.window_geometry.size = Some([1024.0, 768.0]);
            app.session_lyrics_fullscreen_from = left;

            let ctx = egui::Context::default();
            let mut raw_input = egui::RawInput::default();
            raw_input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .fullscreen = Some(true);
            let mut output = ctx.run_ui(raw_input, |_ui| app.attach(&ctx));
            output.textures_delta.clear();
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            let modes: Vec<_> = commands
                .iter()
                .filter(|command| matches!(command, Fullscreen(_) | Maximized(_)))
                .cloned()
                .collect();
            assert_eq!(modes, expected, "{left:?}");
            assert!(
                !commands
                    .iter()
                    .any(|command| matches!(command, InnerSize(_))),
                "{left:?}: the size was the builder's, before the window existed"
            );
            assert_eq!(app.session_lyrics_fullscreen_from, None);
            app.engine.backend.shutdown();
        }
    }

    /// A save that waits for the disk on the UI thread fails debug builds
    /// and tests, so one that would freeze the window cannot come back
    /// unnoticed. (A release build logs it and still writes.)
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "UI thread")]
    fn a_save_that_waits_for_the_disk_is_refused_on_the_ui_thread() {
        let dir = std::env::temp_dir().join(format!(
            "spotifast-ui-thread-save-{:016x}",
            rand::random::<u64>()
        ));
        fastframe_file::mark_ui_thread();
        Settings::default().save(&dir.join("settings.json"));
    }

    /// Settings, session and history saved on the UI thread are handed to
    /// the file writer: the thread never waits for the disk (which panics
    /// in tests), and everything is on the disk after a flush.
    #[test]
    fn saving_from_the_ui_thread_never_waits_for_the_disk() {
        let mut app = test_app("saves-off-the-ui-thread");
        fastframe_file::mark_ui_thread();
        app.engine.settings.theme = ThemeChoice::Light;
        app.engine.plays.record(
            crate::api::models::Track {
                uri: "spotify:track:a".into(),
                ..Default::default()
            },
            jiff::Timestamp::now(),
        );
        app.save_state();
        app.engine.flush_files();
        if let Some(bytes) = app.engine.plays.take_serialized() { app.engine.files.write_private(&app.engine.dirs.history_file(), bytes); }
        assert!(app.engine.flush_files());
        assert_eq!(
            Settings::load(&app.engine.dirs.settings_file()).theme,
            ThemeChoice::Light
        );
        assert!(app.engine.dirs.session_file().exists());
        assert_eq!(
            crate::history::History::load(&app.engine.dirs.history_file())
                .plays()
                .len(),
            1
        );
        app.engine.backend.shutdown();
        let _ = std::fs::remove_dir_all(app.engine.dirs.config.parent().unwrap());
    }

    /// The session remembers fullscreen lyrics and the mode they left, and a
    /// session file from before the field existed still loads.
    #[test]
    fn the_session_remembers_the_mode_fullscreen_lyrics_left() {
        let mut app = test_app("lyrics-fullscreen-session");
        app.engine.lyrics_fullscreen = Some(false);
        app.engine.lyrics_restore_maximized = true;
        app.save_session();
        app.engine.flush_files();
        let session = SessionState::load(&app.engine.dirs.session_file());
        assert_eq!(
            session.lyrics_fullscreen_from,
            Some(crate::settings::WindowMode {
                fullscreen: false,
                maximized: true,
            })
        );

        app.engine.lyrics_fullscreen = None;
        app.save_session();
        app.engine.flush_files();
        let text = std::fs::read_to_string(app.engine.dirs.session_file()).unwrap();
        assert!(!text.contains("lyrics_fullscreen_from"));
        assert_eq!(
            SessionState::load(&app.engine.dirs.session_file()).lyrics_fullscreen_from,
            None
        );
        app.engine.backend.shutdown();
        let _ = std::fs::remove_dir_all(app.engine.dirs.config.parent().unwrap());
    }

    #[test]
    fn a_demo_window_keeps_its_requested_geometry_over_the_saved_session() {
        let mut app = headless_app();
        crate::demo::populate(&mut app);
        app.window_geometry.size = Some([1100.0, 700.0]);
        app.window_geometry.position = Some([100.0, 150.0]);

        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |_ui| app.attach(&ctx));
        output.textures_delta.clear();
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            !commands.iter().any(|command| matches!(
                command,
                egui::ViewportCommand::InnerSize(_) | egui::ViewportCommand::OuterPosition(_)
            )),
            "the last real session must not move or resize the demo: {commands:?}"
        );
        app.engine.backend.shutdown();
    }

    /// The song the last session ended on is shown, paused, at the position
    /// it stopped at, so a cold start does not look like an empty player.
    #[test]
    fn the_remembered_song_is_shown_paused_at_its_position() {
        use crate::api::models::{Album, ArtistRef, Track};
        let mut app = headless_app();
        app.engine.resume_track = Some("spotify:track:abc".into());
        app.engine.resume_position_ms = 19_566;
        assert!(
            app.engine.now_playing().is_none(),
            "nothing to show until the song's details arrive"
        );
        app.engine.track_cache.insert(
            "abc".into(),
            Track {
                id: Some("abc".into()),
                uri: "spotify:track:abc".into(),
                name: "Karma Police".into(),
                artists: vec![ArtistRef {
                    id: None,
                    name: "Radiohead".into(),
                    uri: None,
                }],
                album: Some(Album {
                    id: "ok".into(),
                    name: "OK Computer".into(),
                    ..Default::default()
                }),
                duration_ms: 264_000,
                ..Default::default()
            },
        );
        let now = app.engine.now_playing().expect("the remembered song is shown");
        assert!(now.resuming);
        assert!(!now.playing, "it is shown paused, not played");
        assert_eq!(now.title, "Karma Police");
        assert_eq!(now.subtitle, "Radiohead");
        assert_eq!(now.album_name, "OK Computer");
        assert_eq!(now.duration_ms, 264_000);
        assert_eq!(
            now.position_ms, 19_566,
            "the bar sits where the listener left it, not at zero"
        );
    }

    /// Drawing the remembered song must not be mistaken for a song that
    /// started: that would rewind the very position it exists to show.
    #[test]
    fn showing_the_remembered_song_keeps_its_position() {
        use crate::api::models::Track;
        let mut app = headless_app();
        app.engine.resume_track = Some("spotify:track:abc".into());
        app.engine.resume_position_ms = 19_566;
        app.engine.track_cache.insert(
            "abc".into(),
            Track {
                id: Some("abc".into()),
                uri: "spotify:track:abc".into(),
                duration_ms: 264_000,
                ..Default::default()
            },
        );
        app.on_now_playing_changed();
        assert_eq!(app.engine.resume_position_ms, 19_566);
        app.save_session();
        app.engine.flush_files();
        assert_eq!(
            app.engine.resume_position_ms, 19_566,
            "closing again must not lose the position"
        );
    }

    /// Dragging the bar before pressing play moves where play will land.
    #[test]
    fn seeking_the_remembered_song_moves_the_resume_point() {
        let mut app = headless_app();
        app.engine.resume_track = Some("spotify:track:abc".into());
        app.engine.resume_position_ms = 19_566;
        app.engine.seek(90_000);
        assert_eq!(app.engine.resume_position_ms, 90_000);
    }

    /// Play resumes the remembered track at its saved position.
    #[test]
    fn pressing_play_on_a_cold_start_does_not_restart_the_song() {
        let mut app = headless_app();
        app.engine.resume_context = Some("spotify:playlist:pl1".into());
        app.engine.resume_track = Some("spotify:track:abc".into());
        app.engine.resume_position_ms = 19_566;
        app.engine.toggle_play();
        let request = app.engine.queued_play
            .as_ref()
            .expect("the resumed play is held for the engine");
        assert_eq!(
            request.context_uri.as_deref(),
            Some("spotify:playlist:pl1"),
            "it resumes inside the playlist it was left in"
        );
        assert_eq!(request.offset_uri.as_deref(), Some("spotify:track:abc"));
        assert_eq!(
            request.position_ms, 19_566,
            "the song resumes where it stopped, not at zero"
        );
    }

    /// Play on an in-progress episode continues from the place the row or
    /// card showed as time left. The playing episode stays where it plays.
    #[test]
    fn playing_an_in_progress_episode_continues_from_its_resume_point() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        let in_progress = "spotify:episode:mid";
        let play = |app: &mut App, uri: &str, resume_ms: Option<u32>| {
            app.engine.queued_play = None;
            app.apply(
                Action::PlayEpisode {
                    uri: uri.into(),
                    resume_ms,
                },
                &ctx,
            );
            app.engine.queued_play
                .as_ref()
                .expect("play is held for the engine")
                .position_ms
        };
        assert_eq!(play(&mut app, in_progress, Some(600_000)), 600_000);
        assert_eq!(play(&mut app, "spotify:episode:new", None), 0);

        app.engine.frame_now = Some(NowPlaying {
            local: true,
            device_name: None,
            device_kind: None,
            uri: in_progress.into(),
            id: None,
            title: String::new(),
            artists: Vec::new(),
            subtitle: String::new(),
            album_name: String::new(),
            album_id: None,
            show_id: None,
            art_url: None,
            art_small: None,
            duration_ms: 2_400_000,
            position_ms: 1_000_000,
            playing: true,
            loading: false,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume_percent: 50,
            can_control: true,
            can_set_volume: true,
            is_episode: true,
            resuming: false,
        });
        assert_eq!(
            play(&mut app, in_progress, Some(600_000)),
            0,
            "the playing episode is not sent back to an older saved place"
        );
    }

    /// A started episode resumes where Spotify left it; a finished or
    /// unstarted one starts over, and a place past the end stays inside it.
    #[test]
    fn an_episode_resumes_only_where_it_was_left_unfinished() {
        let episode = |fully_played: bool, resume_position_ms: u32| Episode {
            duration_ms: 2_400_000,
            resume_point: Some(ResumePoint {
                fully_played,
                resume_position_ms,
            }),
            ..Episode::default()
        };
        assert_eq!(episode(false, 900_000).resume_ms(), Some(900_000));
        assert_eq!(episode(true, 2_390_000).resume_ms(), None);
        assert_eq!(episode(false, 0).resume_ms(), None);
        assert_eq!(episode(false, 9_000_000).resume_ms(), Some(2_399_999));
        assert_eq!(Episode::default().resume_ms(), None);
    }

    /// A media key can arrive before startup has reported that the saved
    /// local player is connecting. Its intent must survive that race.
    #[test]
    fn pressing_play_while_startup_connects_is_held_for_the_player() {
        let mut app = headless_app();
        app.engine.local_ready = false;
        app.engine.local_playback = LocalPlayback::Unavailable;
        app.engine.auth = AuthStatus::Starting;
        app.engine.settings.playback_authorized = true;
        app.engine.resume_track = Some("spotify:track:abc".into());

        app.engine.toggle_play();

        assert!(app.engine.queued_play.is_some());
        assert!(!app.engine.show_devices, "startup is not a missing-device error");
    }

    /// Previous and Next move a restored track without starting playback.
    #[test]
    fn the_transport_works_on_a_restored_song_without_playing_it() {
        use crate::api::models::{PlayableItem, PlaylistItem, Track};
        use crate::model::PagedList;
        let row = |uri: &str| PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                id: Some(uri.rsplit(':').next().unwrap().into()),
                uri: uri.into(),
                name: uri.into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.playlist_pages.insert(
            "pl1".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        row("spotify:track:one"),
                        row("spotify:track:two"),
                        row("spotify:track:three"),
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.engine.resume_context = Some("spotify:playlist:pl1".into());
        app.engine.resume_track = Some("spotify:track:two".into());
        app.engine.resume_position_ms = 19_566;
        assert!(app.engine.resume_only(), "loaded and current, but not playing");

        // Next steps to the following song, at its start, still not playing.
        app.apply(Action::Next, &ctx);
        assert_eq!(app.engine.resume_track.as_deref(), Some("spotify:track:three"));
        assert_eq!(app.engine.resume_position_ms, 0);
        assert!(
            app.engine.queued_play.is_none() && app.engine.local_list.is_none(),
            "skipping must not start the restored song"
        );
        // Use loaded row details for immediate display.
        let now = app.engine.now_playing().expect("the new song is shown");
        assert!(now.resuming && !now.playing);
        assert_eq!(now.uri, "spotify:track:three");

        // Previous steps back from the start of a song.
        app.apply(Action::Previous, &ctx);
        assert_eq!(app.engine.resume_track.as_deref(), Some("spotify:track:two"));

        // Past the threshold, Previous restarts instead, as it does while
        // playing.
        app.engine.resume_position_ms = 19_566;
        app.apply(Action::Previous, &ctx);
        assert_eq!(app.engine.resume_track.as_deref(), Some("spotify:track:two"));
        assert_eq!(app.engine.resume_position_ms, 0, "it restarts the song");

        // The ends of the list wrap rather than dead-ending.
        app.apply(Action::Previous, &ctx);
        assert_eq!(app.engine.resume_track.as_deref(), Some("spotify:track:one"));
        app.apply(Action::Previous, &ctx);
        assert_eq!(app.engine.resume_track.as_deref(), Some("spotify:track:three"));

        // Play starts the selected track in its playlist.
        app.apply(Action::TogglePlay, &ctx);
        let request = app.engine.queued_play.as_ref().expect("play starts it");
        assert_eq!(
            request.context_uri.as_deref(),
            Some("spotify:playlist:pl1"),
            "the playlist it was left in is kept"
        );
        assert_eq!(request.offset_uri.as_deref(), Some("spotify:track:three"));
    }

    /// A restored session keeps shuffle enabled when skipping.
    #[test]
    fn skipping_a_restored_song_keeps_shuffle_on() {
        use crate::api::models::{PlayableItem, PlaylistItem, Track};
        use crate::model::PagedList;
        let row = |uri: &str| PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                uri: uri.into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.playlist_pages.insert(
            "pl1".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![row("spotify:track:one"), row("spotify:track:two")],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.engine.resume_context = Some("spotify:playlist:pl1".into());
        app.engine.resume_track = Some("spotify:track:one".into());
        app.engine.shuffle_wanted = true;
        app.apply(Action::Next, &ctx);
        assert!(app.engine.shuffle_wanted, "shuffle survives the skip");
        assert_eq!(
            app.engine.resume_track.as_deref(),
            Some("spotify:track:two"),
            "a shuffled skip still lands on another song in the context"
        );
    }

    /// The saved queue is restored only with its remembered track.
    #[test]
    fn the_saved_queue_follows_the_resumed_song_only() {
        let mut app = headless_app();
        app.engine.resume_track = Some("spotify:track:abc".into());
        app.engine.resume_queue = vec!["spotify:track:q1".into(), "spotify:track:q2".into()];
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:other".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.on_now_playing_changed();
        assert!(
            app.engine.resume_queue.is_empty(),
            "a fresh start lets the saved queue go"
        );
        assert!(app.engine.session_dirty);
    }

    /// A selected list row loads without shuffle, then enables shuffle.
    #[test]
    fn a_chosen_row_in_a_list_never_loads_shuffled() {
        let request = PlayRequest::tracks(vec!["spotify:track:a".into(), "spotify:track:b".into()])
            .starting_at_index(1);
        let load = local_load(&request, true);
        assert_eq!(load.shuffle, None);
        assert_eq!(load.offset_index, Some(1));
        // Without a chosen row the list may shuffle from the start.
        let request = PlayRequest::tracks(vec!["spotify:track:a".into(), "spotify:track:b".into()]);
        assert_eq!(local_load(&request, true).shuffle, Some(true));
        // A context play keeps its shuffled load; the offset was already
        // picked to match.
        let request = PlayRequest::context("spotify:playlist:x").starting_at_uri("spotify:track:a");
        assert_eq!(local_load(&request, true).shuffle, Some(true));
    }

    fn queued_song(uri: &str) -> crate::api::models::PlayableItem {
        crate::api::models::PlayableItem::Track(crate::api::models::Track {
            uri: uri.into(),
            ..Default::default()
        })
    }

    fn loaded_queue(current: &str, next: &[&str]) -> Loadable<Queue> {
        Loadable::Loaded(Queue {
            currently_playing: Some(queued_song(current)),
            queue: next.iter().map(|uri| queued_song(uri)).collect(),
        })
    }

    fn queue_uris(app: &App) -> (Option<String>, Vec<String>) {
        let queue = app.engine.queue.get().expect("the queue stays loaded");
        (
            queue
                .currently_playing
                .as_ref()
                .map(|item| item.uri().to_string()),
            queue
                .queue
                .iter()
                .map(|item| item.uri().to_string())
                .collect(),
        )
    }

    /// Next moves the queue head to the playing row immediately.
    #[test]
    fn next_pops_the_queue_head_into_now_playing() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:b", "spotify:track:c"]);
        app.apply(Action::Next, &ctx);
        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:b"));
        assert_eq!(next, vec!["spotify:track:c"]);
        assert_eq!(
            app.engine.current_track_uri().as_deref(),
            Some("spotify:track:b"),
            "the popped row is already the one the interface marks as playing"
        );
    }

    /// Next names the next song in the player bar at once, from the queue
    /// row, even when the song was never loaded anywhere else.
    #[test]
    fn next_shows_the_next_songs_title_before_the_engine_loads_it() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = Loadable::Loaded(Queue {
            currently_playing: Some(queued_song("spotify:track:a")),
            queue: vec![crate::api::models::PlayableItem::Track(Track {
                id: Some("b".into()),
                uri: "spotify:track:b".into(),
                name: "Second Song".into(),
                ..Default::default()
            })],
        });
        assert!(!app.engine.track_cache.contains_key("b"));
        app.apply(Action::Next, &egui::Context::default());
        let now = app.engine.now_playing().expect("the next song is on show");
        assert_eq!(now.uri, "spotify:track:b");
        assert_eq!(now.title, "Second Song");
    }

    #[test]
    fn a_pending_next_keeps_paused_playback_paused() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Paused;
        app.engine.track_cache.insert(
            "b".into(),
            Track {
                uri: "spotify:track:b".into(),
                ..Default::default()
            },
        );
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:b"]);
        app.apply(Action::Next, &egui::Context::default());
        let now = app.engine.now_playing().unwrap();
        assert_eq!(now.uri, "spotify:track:b");
        assert!(
            !now.playing,
            "Next preserves pause while the engine catches up"
        );
    }

    /// A queue restored at startup is useful to show, but it is not evidence
    /// of what follows the player now and must not drive an optimistic skip.
    #[test]
    fn next_does_not_guess_from_an_unanchored_queue() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:honey".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = Loadable::Loaded(Queue {
            currently_playing: None,
            queue: vec![queued_song("spotify:track:stale")],
        });

        app.apply(Action::Next, &ctx);

        assert_eq!(
            app.engine.current_track_uri().as_deref(),
            Some("spotify:track:honey"),
            "an unanchored row is not presented as Next's destination"
        );
        assert!(app.engine.intent_track.is_none());
        assert_eq!(
            queue_uris(&app).1,
            vec!["spotify:track:stale"],
            "the restored queue stays visible until the live queue arrives"
        );
    }

    /// The queue can still disagree with librespot after an anchored skip.
    /// Once the engine reports the track that started, its report is final.
    #[test]
    fn an_engine_track_report_overrules_the_optimistic_queue_marker() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:honey".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue("spotify:track:honey", &["spotify:track:guessed"]);

        app.apply(Action::Next, &ctx);
        assert_eq!(
            app.engine.current_track_uri().as_deref(),
            Some("spotify:track:guessed")
        );

        let mut reported = app.engine.local.clone();
        reported.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:black-dove".into(),
            ..Default::default()
        });
        app.handle_local(reported);

        assert_eq!(
            app.engine.current_track_uri().as_deref(),
            Some("spotify:track:black-dove"),
            "the real engine track wins without waiting for a timeout"
        );
        assert!(app.engine.intent_track.is_none());
    }

    /// Remote state is polled rather than pushed. Ignore a poll already in
    /// flight and one lagging answer, then accept a second report.
    #[test]
    fn remote_track_intent_waits_for_a_fresh_confirming_poll() {
        let mut app = headless_app();
        app.engine.local_ready = false;
        app.engine.selected_device = Some("phone".into());
        app.engine.remote_poll_seq = 7;
        app.engine.expect_track("spotify:track:wanted".into(), 0);

        app.engine.reconcile_remote_track_intent(7, Some("spotify:track:old"));
        assert!(
            app.engine.intent_track.is_some(),
            "the in-flight poll predates the command"
        );

        app.engine.reconcile_remote_track_intent(8, Some("spotify:track:old"));
        assert!(
            app.engine.intent_track.is_some(),
            "one stale Spotify answer is retried"
        );

        app.engine.reconcile_remote_track_intent(9, Some("spotify:track:actual"));
        assert!(
            app.engine.intent_track.is_none(),
            "the second fresh report settles playback"
        );
    }

    /// Going back cancels Next's speculative destination, even when both
    /// commands arrive before the engine has reported either track change.
    #[test]
    fn previous_discards_an_unconfirmed_next_marker() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:b"]);

        app.apply(Action::Next, &ctx);
        assert_eq!(
            app.engine.current_track_uri().as_deref(),
            Some("spotify:track:b"),
            "Next marks its queue head immediately"
        );

        app.apply(Action::Previous, &ctx);
        assert_eq!(
            app.engine.current_track_uri().as_deref(),
            Some("spotify:track:a"),
            "Previous restores the engine's current row instead of holding Next's marker"
        );
    }

    /// A track is removed from Next up as soon as it starts.
    #[test]
    fn a_song_starting_consumes_its_queue_row() {
        let mut app = headless_app();
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:b", "spotify:track:c"]);
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:b".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.on_now_playing_changed();
        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:b"));
        assert_eq!(next, vec!["spotify:track:c"]);
    }

    /// A stale queue response does not undo an optimistic skip.
    #[test]
    fn a_stale_queue_answer_does_not_undo_a_skip() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:b", "spotify:track:c"]);
        app.apply(Action::Next, &ctx);

        let stale = Queue {
            currently_playing: Some(queued_song("spotify:track:a")),
            queue: vec![
                queued_song("spotify:track:b"),
                queued_song("spotify:track:c"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: app.engine.queue_seq,
            result: Ok(stale.clone()),
        });
        let (current, next) = queue_uris(&app);
        assert_eq!(
            current.as_deref(),
            Some("spotify:track:b"),
            "the pop stands"
        );
        assert_eq!(next, vec!["spotify:track:c"]);
        assert!(
            app.engine.queue_recheck_at.is_some(),
            "the stale answer is asked again rather than believed"
        );

        // Accept Spotify's state after the retry limit.
        for _ in 0..QUEUE_STALE_RETRIES {
            app.handle_api(ApiResponse::Queue {
                seq: app.engine.queue_seq,
                result: Ok(stale.clone()),
            });
        }
        let (current, _) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:a"));
    }

    /// A pending addition is not restored after its track starts.
    #[test]
    fn a_played_pending_add_is_not_put_back() {
        let mut app = headless_app();
        app.engine.pending_queue_adds = vec![PendingQueueAdd {
            item: queued_song("spotify:track:b"),
            at: Instant::now(),
            manual_index: 0,
            write: None,
        }];
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:b".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue("spotify:track:b", &["spotify:track:c"]);
        app.engine.reconcile_pending_queue();
        let (_, next) = queue_uris(&app);
        assert_eq!(next, vec!["spotify:track:c"], "no resurrected row on top");
        assert!(
            app.engine.pending_queue_adds.is_empty(),
            "the add has been consumed"
        );
    }

    /// Playing a queue row consumes it and every row before it.
    #[test]
    fn a_chosen_queue_row_plays_at_once_and_takes_the_rows_above() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.manual_queue = vec!["spotify:track:b".into(), "spotify:track:c".into()];
        app.engine.queue = loaded_queue(
            "spotify:track:a",
            &["spotify:track:b", "spotify:track:c", "spotify:track:d"],
        );
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Queue,
                uri: "spotify:track:c".into(),
                index: 1,
            },
            &ctx,
        );
        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:c"));
        assert_eq!(
            next,
            vec!["spotify:track:d"],
            "the rows after the chosen one stay"
        );
        assert_eq!(
            app.engine.current_track_uri().as_deref(),
            Some("spotify:track:c"),
            "the chosen row is marked as playing at once"
        );
        assert!(
            app.engine.manual_queue.is_empty(),
            "hand-queued songs consumed by the jump are let go"
        );
        assert!(app.engine.play_pending("spotify:track:c"));
    }

    /// The click names a song: when the rows have shifted under the
    /// pointer, the song wins over the row number.
    #[test]
    fn a_clicked_queue_row_is_found_by_its_song_when_rows_shifted() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:b", "spotify:track:c"]);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Queue,
                uri: "spotify:track:c".into(),
                index: 0,
            },
            &ctx,
        );
        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:c"));
        assert!(
            next.is_empty(),
            "the row above the chosen song went with it"
        );
    }

    /// Clear queue removes manual rows and preserves matching context rows.
    #[test]
    fn clear_queue_takes_the_hand_queued_rows_and_keeps_the_context() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.manual_queue = vec!["spotify:track:b".into(), "spotify:track:c".into()];
        app.engine.queue = loaded_queue(
            "spotify:track:a",
            &[
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:c",
                "spotify:track:d",
            ],
        );
        assert!(app.engine.can_clear_queue());
        app.apply(Action::ClearQueue, &ctx);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:c", "spotify:track:d"],
            "one queued c goes, the context's own c stays"
        );
        assert!(app.engine.manual_queue.is_empty());
        assert!(
            app.engine.queue_recheck_at.is_some(),
            "a fetch follows to sweep rows queued from other devices"
        );
    }

    /// Rule: clearing takes back the rows Add to queue added, and the
    /// context's own copy of the same song is not one of them, however
    /// recently the song was queued.
    #[test]
    fn clearing_a_just_queued_song_leaves_the_contexts_copy_of_it() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        // What is playing comes to b on its own later on.
        app.engine.queue = loaded_queue(
            "spotify:track:a",
            &[
                "spotify:track:ctx1",
                "spotify:track:b",
                "spotify:track:ctx2",
            ],
        );
        app.apply(
            Action::AddToQueue {
                uri: "spotify:track:b".into(),
                label: "b".into(),
            },
            &ctx,
        );
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:b",
                "spotify:track:ctx1",
                "spotify:track:b",
                "spotify:track:ctx2",
            ],
            "one row queued on top, the context's own b still below"
        );
        assert!(app.engine.can_clear_queue());
        app.apply(Action::ClearQueue, &ctx);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:ctx1",
                "spotify:track:b",
                "spotify:track:ctx2",
            ],
            "the queued b goes and the context keeps the b it was going to play"
        );
    }

    /// Add to queue inserts after manual queue rows and before context rows.
    #[test]
    fn play_next_queues_after_the_songs_already_queued() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue(
            "spotify:track:a",
            &["spotify:track:ctx1", "spotify:track:ctx2"],
        );
        app.apply(
            Action::AddToQueue {
                uri: "spotify:track:b".into(),
                label: "b".into(),
            },
            &ctx,
        );
        app.apply(
            Action::AddToQueue {
                uri: "spotify:track:c".into(),
                label: "c".into(),
            },
            &ctx,
        );
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ],
            "queued songs keep their order and stay ahead of the context"
        );
        assert_eq!(
            app.engine.toasts.last().map(|toast| toast.message.as_str()),
            Some("c added to queue")
        );
    }

    fn album_queue_track(id: &str) -> Track {
        Track {
            uri: format!("spotify:track:{id}"),
            name: id.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn starting_a_queued_album_preserves_its_manual_copies_and_clear_button() {
        let ctx = egui::Context::default();
        for advance in ["clear", "next", "natural"] {
            let mut app = headless_app();
            app.engine.local.connected = true;
            app.engine.local.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:old".into(),
                ..Default::default()
            });
            app.engine.local.playback = Playback::Playing;
            app.engine.queue = loaded_queue("spotify:track:old", &["spotify:track:old-next"]);
            app.on_now_playing_changed();
            app.engine.album_pages.insert(
                "album".into(),
                AlbumPage {
                    tracks: PagedList {
                        items: ["intro", "second", "third"]
                            .into_iter()
                            .map(album_queue_track)
                            .collect(),
                        loaded_once: true,
                        next_offset: None,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            app.apply(
                Action::AddToQueue {
                    uri: "spotify:album:album".into(),
                    label: "Album".into(),
                },
                &ctx,
            );
            app.engine.add_to_queue("spotify:track:extra".into(), "Extra".into());
            let manual = app.engine.manual_queue.clone();
            assert_eq!(manual.len(), 4);
            app.apply(
                Action::PlayContext {
                    uri: "spotify:album:album".into(),
                    offset_uri: None,
                    offset_index: None,
                },
                &ctx,
            );
            let mut started = app.engine.local.clone();
            started.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:intro".into(),
                ..Default::default()
            });
            started.track_sequence += 1;
            app.handle_local(started);
            assert_eq!(
                app.engine.manual_queue, manual,
                "the album start is not a queued play"
            );

            let next = [
                "spotify:track:intro",
                "spotify:track:second",
                "spotify:track:third",
                "spotify:track:extra",
                "spotify:track:second",
                "spotify:track:third",
            ];
            let Loadable::Loaded(fetched) = loaded_queue("spotify:track:intro", &next) else {
                unreachable!();
            };
            app.handle_api(ApiResponse::Queue {
                seq: app.engine.queue_seq,
                result: Ok(fetched),
            });
            assert_eq!(queue_uris(&app).1, next);
            assert_eq!(app.engine.queued_rows_len(), 4);
            assert!(app.engine.can_clear_queue());

            // Restarting the same album also keeps its separate queued copy.
            app.apply(
                Action::PlayContext {
                    uri: "spotify:album:album".into(),
                    offset_uri: None,
                    offset_index: None,
                },
                &ctx,
            );
            let mut restarted = app.engine.local.clone();
            restarted.track_sequence += 1;
            app.handle_local(restarted);
            assert_eq!(app.engine.manual_queue, manual);
            assert_eq!(queue_uris(&app).1, next);

            if advance != "clear" {
                if advance == "next" {
                    app.apply(Action::Next, &ctx);
                    assert_eq!(app.engine.queued_rows_len(), 3);
                }
                // The queued intro has the same URI and metadata as the album's
                // intro. Both Next and natural completion consume it once.
                let mut queued_intro = app.engine.local.clone();
                queued_intro.track_sequence += 1;
                app.handle_local(queued_intro);
                assert_eq!(app.engine.manual_queue, manual[1..]);
                assert_eq!(queue_uris(&app).1, next[1..]);
                assert_eq!(app.engine.queued_rows_len(), 3);
                assert!(app.engine.can_clear_queue());
            }

            app.apply(Action::ClearQueue, &ctx);
            assert_eq!(queue_uris(&app).1, next[4..]);
            assert!(!app.engine.can_clear_queue());
            app.engine.backend.shutdown();
        }
    }

    #[test]
    fn replaying_a_song_only_consumes_a_queue_copy_when_the_queue_advances() {
        let ctx = egui::Context::default();
        for repeat in [RepeatMode::Off, RepeatMode::Track] {
            let mut app = headless_app();
            app.engine.local = LocalState {
                connected: true,
                playback: Playback::Playing,
                repeat,
                track: Some(crate::player::LocalTrack {
                    uri: "spotify:track:a".into(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            app.on_now_playing_changed();
            app.engine.manual_queue = vec!["spotify:track:b".into(), "spotify:track:b".into()];
            app.engine.queue = loaded_queue(
                "spotify:track:a",
                &[
                    "spotify:track:b",
                    "spotify:track:b",
                    "spotify:track:context",
                ],
            );
            app.apply(Action::Next, &ctx);
            let mut started = app.engine.local.clone();
            started.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:b".into(),
                ..Default::default()
            });
            started.track_sequence += 1;
            app.handle_local(started);
            assert_eq!(app.engine.manual_queue, ["spotify:track:b"]);
            assert_eq!(app.engine.queued_rows_len(), 1);

            let mut again = app.engine.local.clone();
            again.track_sequence += 1;
            app.handle_local(again);
            if repeat == RepeatMode::Track {
                assert_eq!(app.engine.manual_queue, ["spotify:track:b"]);
                assert_eq!(
                    app.engine.queued_rows_len(),
                    1,
                    "repeat does not advance the queue"
                );
                app.apply(Action::Next, &ctx);
                let mut skipped = app.engine.local.clone();
                skipped.track_sequence += 1;
                app.handle_local(skipped);
            }
            assert!(app.engine.manual_queue.is_empty());
            assert_eq!(queue_uris(&app).1, ["spotify:track:context"]);
            app.engine.backend.shutdown();
        }
    }

    #[test]
    fn queue_album_uses_every_cached_song_in_order_and_preserves_duplicates() {
        for local in [false, true] {
            let mut app = test_app(if local {
                "album-queue-local"
            } else {
                "album-queue-remote"
            });
            app.engine.local_ready = local;
            app.engine.local.playback = Playback::Playing;
            app.engine.local.connected = local;
            app.engine.local.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:current".into(),
                ..Default::default()
            });
            app.engine.queue = loaded_queue(
                "spotify:track:current",
                &["spotify:track:manual", "spotify:track:context"],
            );
            app.engine.manual_queue.push("spotify:track:manual".into());
            let mut tracks: Vec<_> = (0..120)
                .map(|n| album_queue_track(&format!("song{n}")))
                .collect();
            tracks.push(album_queue_track("song0"));
            let expected: Vec<_> = tracks.iter().map(|track| track.uri.clone()).collect();
            tracks.push(Track {
                is_playable: Some(false),
                ..album_queue_track("unavailable")
            });
            tracks.push(Track::default());
            tracks.push(Track {
                is_local: true,
                ..album_queue_track("local")
            });
            app.engine.album_pages.insert(
                "album".into(),
                AlbumPage {
                    tracks: PagedList {
                        items: tracks,
                        loaded_once: true,
                        next_offset: None,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            let action = Action::AddToQueue {
                uri: "spotify:album:album".into(),
                label: "Album".into(),
            };
            app.apply(action.clone(), &egui::Context::default());
            app.apply(action, &egui::Context::default());
            let (_, queued) = queue_uris(&app);
            assert_eq!(queued[0], "spotify:track:manual");
            assert_eq!(queued[1..queued.len() - 1], expected);
            assert_eq!(queued.last().unwrap(), "spotify:track:context");
            if local {
                assert_eq!(app.engine.backend.take_queued_tracks(), expected);
                assert!(app.engine.backend.take_queue_requests().is_empty());
            } else {
                let requests = app.engine.backend.take_queue_requests();
                assert_eq!(requests.len(), 1);
                assert!(
                    matches!(&requests[0], ApiRequest::AddManyToQueue { uris, .. } if uris == &expected)
                );
            }
            assert!(app.engine.pending_album_queues.is_empty());
            app.engine.add_to_queue("spotify:track:after".into(), "After".into());
            let (_, queued) = queue_uris(&app);
            assert_eq!(
                queued[queued.len() - 2],
                "spotify:track:after",
                "a later song stays after an album longer than 100 tracks"
            );
            app.engine.backend.shutdown();
        }
    }

    #[test]
    fn queue_album_fetches_all_pages_before_appending_and_ignores_old_pages() {
        let mut app = test_app("album-queue-pages");
        app.engine.queue = loaded_queue("spotify:track:current", &["spotify:track:context"]);
        app.engine.add_to_queue("spotify:album:album".into(), "Album".into());
        let request = app.engine.album_queue_serial;
        let requests = app.engine.backend.take_queue_requests();
        assert!(matches!(
            &requests[..],
            [ApiRequest::AlbumQueueTracks { offset: 0, .. }]
        ));
        let page = crate::api::models::Page {
            items: vec![album_queue_track("one"), album_queue_track("two")],
            offset: 0,
            limit: 2,
            total: 3,
            next: Some("next".into()),
        };
        app.handle_api(ApiResponse::AlbumQueueTracks {
            request,
            offset: 0,
            result: Ok(page.clone()),
        });
        assert_eq!(queue_uris(&app).1, ["spotify:track:context"]);
        let requests = app.engine.backend.take_queue_requests();
        assert!(matches!(
            &requests[..],
            [ApiRequest::AlbumQueueTracks { offset: 2, .. }]
        ));
        app.handle_api(ApiResponse::AlbumQueueTracks {
            request,
            offset: 0,
            result: Ok(page),
        });
        assert!(app.engine.backend.take_queue_requests().is_empty());
        app.handle_api(ApiResponse::AlbumQueueTracks {
            request,
            offset: 2,
            result: Ok(crate::api::models::Page {
                items: vec![album_queue_track("one")],
                offset: 2,
                limit: 2,
                total: 3,
                next: None,
            }),
        });
        assert_eq!(
            queue_uris(&app).1,
            [
                "spotify:track:one",
                "spotify:track:two",
                "spotify:track:one",
                "spotify:track:context"
            ]
        );
        assert!(app.engine.pending_album_queues.is_empty());
        app.engine.backend.shutdown();
    }

    #[test]
    fn remote_album_queue_keeps_its_rows_while_rate_limited_writes_are_pending() {
        let mut app = test_app("album-queue-stale");
        app.engine.queue = loaded_queue("spotify:track:current", &["spotify:track:context"]);
        let old = app.engine.queue.get().unwrap().clone();
        app.engine.queue_album_tracks(
            vec![
                album_queue_track("a"),
                album_queue_track("b"),
                album_queue_track("a"),
            ],
            "Album".into(),
        );
        let request = app.engine.album_queue_serial;
        for addition in &mut app.engine.pending_queue_adds {
            addition.at = Instant::now() - Duration::from_secs(60);
        }
        app.engine.expire_pending_queue_adds();
        assert_eq!(app.engine.pending_queue_adds.len(), 3);
        for _ in 0..u16::from(u8::MAX) + 2 {
            app.handle_api(ApiResponse::Queue {
                seq: app.engine.queue_seq,
                result: Ok(old.clone()),
            });
            assert_eq!(
                queue_uris(&app).1,
                [
                    "spotify:track:a",
                    "spotify:track:b",
                    "spotify:track:a",
                    "spotify:track:context"
                ]
            );
        }
        app.handle_api(ApiResponse::Queue {
            seq: app.engine.queue_seq,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(
            queue_uris(&app).1,
            [
                "spotify:track:a",
                "spotify:track:b",
                "spotify:track:a",
                "spotify:track:context"
            ]
        );
        app.handle_api(ApiResponse::QueueBatchAdded {
            request,
            added: 3,
            result: Ok(()),
        });
        assert!(app.engine.pending_queue_batches.is_empty());
        assert!(
            app.engine.pending_queue_adds
                .iter()
                .all(|addition| addition.at.elapsed() < Duration::from_secs(1))
        );
        app.engine.backend.shutdown();
    }

    #[test]
    fn rejected_queue_copies_leave_accepted_later_and_context_copies_intact() {
        let mut app = test_app("queue-partial-failure");
        app.engine.queue = loaded_queue("spotify:track:current", &["spotify:track:a"]);
        app.engine.queue_album_tracks(
            vec![
                album_queue_track("a"),
                album_queue_track("a"),
                album_queue_track("b"),
            ],
            "Album".into(),
        );
        let album = app.engine.album_queue_serial;
        // Bypass click debounce: this is a separately selected occurrence.
        app.engine.queue_one("spotify:track:a".into(), "Later A".into(), false);
        let later = app.engine.album_queue_serial;
        app.handle_api(ApiResponse::QueueBatchAdded {
            request: album,
            added: 1,
            result: Err(crate::api::ApiError::Status {
                status: 403,
                message: "Rejected".into(),
            }),
        });
        assert_eq!(app.engine.manual_queue, ["spotify:track:a", "spotify:track:a"]);
        assert_eq!(
            queue_uris(&app).1,
            ["spotify:track:a", "spotify:track:a", "spotify:track:a"]
        );
        assert_eq!(app.engine.pending_queue_adds.len(), 2);
        assert_eq!(app.engine.pending_queue_adds[1].manual_index, 1);
        assert_eq!(app.engine.pending_queue_adds[1].write, Some((later, 0)));
        app.handle_api(ApiResponse::QueueBatchAdded {
            request: later,
            added: 0,
            result: Err(crate::api::ApiError::QuotaExhausted),
        });
        assert_eq!(app.engine.manual_queue, ["spotify:track:a"]);
        assert_eq!(queue_uris(&app).1, ["spotify:track:a", "spotify:track:a"]);
        app.engine.backend.shutdown();
    }

    #[test]
    fn pending_single_keeps_its_name_and_duration_during_and_after_a_cooldown() {
        let mut app = test_app("queue-single-cooldown");
        app.engine.queue = loaded_queue("spotify:track:current", &["spotify:track:context"]);
        let old = app.engine.queue.get().unwrap().clone();
        let track = Track {
            duration_ms: 215_000,
            ..album_queue_track("single")
        };
        app.engine.album_pages.insert(
            "album".into(),
            AlbumPage {
                tracks: PagedList {
                    items: vec![track.clone()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.engine.add_to_queue(track.uri.clone(), track.name.clone());
        let request = app.engine.album_queue_serial;
        app.engine.album_pages.clear();
        app.engine.track_cache.clear();
        app.engine.pending_queue_adds[0].at = Instant::now() - Duration::from_secs(90);
        app.handle_api(ApiResponse::Queue {
            seq: app.engine.queue_seq,
            result: Ok(old.clone()),
        });
        let row = &app.engine.queue.get().unwrap().queue[0];
        assert_eq!(row.name(), track.name);
        assert_eq!(row.duration_ms(), track.duration_ms);
        app.handle_api(ApiResponse::QueueBatchAdded {
            request,
            added: 1,
            result: Ok(()),
        });
        app.handle_api(ApiResponse::Queue {
            seq: app.engine.queue_seq,
            result: Ok(old),
        });
        let row = &app.engine.queue.get().unwrap().queue[0];
        assert_eq!(row.uri(), track.uri);
        assert_eq!(row.name(), track.name);
        assert_eq!(row.duration_ms(), track.duration_ms);
        app.engine.backend.shutdown();
    }

    #[test]
    fn pending_album_queue_cannot_survive_clear_sign_out_or_device_change() {
        for cancel in ["clear", "sign-out", "device", "failure"] {
            let mut app = test_app(&format!("album-queue-cancel-{cancel}"));
            app.engine.local_ready = true;
            app.engine.queue = loaded_queue("spotify:track:current", &["spotify:track:context"]);
            app.engine.add_to_queue("spotify:album:album".into(), "Album".into());
            let request = app.engine.album_queue_serial;
            app.engine.backend.take_queue_requests();
            match cancel {
                "clear" => app.engine.clear_queue(),
                "sign-out" => app.engine.reset_data(),
                "device" => app.engine.selected_device = Some("phone".into()),
                "failure" => app.handle_api(ApiResponse::AlbumQueueTracks {
                    request,
                    offset: 0,
                    result: Err(crate::api::ApiError::RateLimited),
                }),
                _ => unreachable!(),
            }
            app.handle_api(ApiResponse::AlbumQueueTracks {
                request,
                offset: 0,
                result: Ok(crate::api::models::Page {
                    items: vec![album_queue_track("late")],
                    ..Default::default()
                }),
            });
            assert!(app.engine.manual_queue.is_empty());
            assert!(app.engine.pending_album_queues.is_empty());
            assert!(app.engine.backend.take_queue_requests().is_empty());
            assert!(app.engine.backend.take_queued_tracks().is_empty());
            app.engine.backend.shutdown();
        }
    }

    /// Separate requests may queue duplicates; duplicate click events do not.
    #[test]
    fn asking_play_next_twice_queues_two_rows() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:ctx1"]);
        let add = Action::AddToQueue {
            uri: "spotify:track:b".into(),
            label: "b".into(),
        };
        app.apply(add.clone(), &ctx);
        app.apply(add.clone(), &ctx);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b", "spotify:track:ctx1"],
            "the double-click's second click is not a second wish"
        );
        // Simulate a later request.
        for addition in &mut app.engine.pending_queue_adds {
            addition.at = Instant::now() - QUEUE_ADD_DEBOUNCE;
        }
        app.apply(add, &ctx);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b", "spotify:track:b", "spotify:track:ctx1"],
            "two asks are two rows, one after the other"
        );
    }

    /// A playlist can hold the same song twice. Picking both rows and
    /// choosing Add to queue is one ask for each row, so the song is queued
    /// twice, as the notification says.
    #[test]
    fn a_song_picked_twice_is_queued_twice() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:ctx1"]);
        let add = Action::QueueMany {
            songs: vec![
                ("spotify:track:b".into(), "b".into()),
                ("spotify:track:c".into(), "c".into()),
                ("spotify:track:b".into(), "b".into()),
            ],
        };
        app.apply(add.clone(), &ctx);
        // The same click arriving twice is still one ask.
        let toasts = app.engine.toasts.len();
        app.apply(add.clone(), &ctx);
        assert_eq!(app.engine.toasts.len(), toasts, "no second addition to announce");
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:b",
                "spotify:track:ctx1",
            ],
            "every picked row is queued, in order"
        );
        assert_eq!(
            app.engine.manual_queue,
            vec!["spotify:track:b", "spotify:track:c", "spotify:track:b"]
        );
        assert_eq!(
            app.engine.toasts.last().map(|toast| toast.message.as_str()),
            Some("3 songs added to queue")
        );
        // A later request is separate and must preserve its duplicates too.
        for addition in &mut app.engine.pending_queue_adds {
            addition.at = Instant::now() - QUEUE_ADD_DEBOUNCE;
        }
        app.apply(add, &ctx);
        assert_eq!(
            queue_uris(&app).1,
            [
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:b",
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:b",
                "spotify:track:ctx1"
            ]
        );
    }

    #[test]
    fn a_queue_batch_reports_only_rows_it_added() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:ctx1"]);
        app.apply(
            Action::AddToQueue {
                uri: "spotify:track:b".into(),
                label: "b".into(),
            },
            &ctx,
        );
        app.apply(
            Action::QueueMany {
                songs: vec![
                    ("spotify:track:b".into(), "b".into()),
                    ("spotify:track:c".into(), "c".into()),
                    ("spotify:track:c".into(), "c".into()),
                ],
            },
            &ctx,
        );
        assert_eq!(
            queue_uris(&app).1,
            [
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:c",
                "spotify:track:ctx1"
            ]
        );
        assert_eq!(app.engine.toasts.last().unwrap().message, "2 songs added to queue");
        let toasts = app.engine.toasts.len();
        app.apply(Action::QueueMany { songs: vec![] }, &ctx);
        assert_eq!(app.engine.toasts.len(), toasts, "an empty batch adds nothing");
    }

    /// A response superseded by a newer request is ignored.
    #[test]
    fn an_overtaken_queue_answer_is_dropped_unread() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:b"]);
        app.engine.queue_seq = 2;
        let old_story = Queue {
            currently_playing: Some(queued_song("spotify:track:a")),
            queue: Vec::new(),
        };
        app.handle_api(ApiResponse::Queue {
            seq: 1,
            result: Ok(old_story),
        });
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b"],
            "the overtaken answer changed nothing"
        );
        let current_story = Queue {
            currently_playing: Some(queued_song("spotify:track:a")),
            queue: vec![
                queued_song("spotify:track:b"),
                queued_song("spotify:track:c"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: 2,
            result: Ok(current_story),
        });
        let (_, next) = queue_uris(&app);
        assert_eq!(next, vec!["spotify:track:b", "spotify:track:c"]);
    }

    /// Rule: a row you queued is put back until Spotify confirms it, in
    /// its place after the queued section, not on top of it.
    #[test]
    fn a_missing_queued_row_returns_to_its_place() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.manual_queue = vec!["spotify:track:b".into(), "spotify:track:c".into()];
        app.engine.pending_queue_adds = vec![PendingQueueAdd {
            item: queued_song("spotify:track:c"),
            at: Instant::now(),
            manual_index: 1,
            write: None,
        }];
        // Spotify's answer knows b already but not c yet.
        app.engine.queue = loaded_queue(
            "spotify:track:a",
            &["spotify:track:b", "spotify:track:ctx1"],
        );
        app.engine.reconcile_pending_queue();
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b", "spotify:track:c", "spotify:track:ctx1"],
            "the missing row comes back after the queued section"
        );
    }

    /// The view splits the queue where the user's own songs end; rows
    /// queued elsewhere or belonging to the context stay below the line.
    #[test]
    fn the_queued_section_covers_only_the_users_own_rows() {
        let mut app = headless_app();
        app.engine.manual_queue = vec!["spotify:track:b".into(), "spotify:track:c".into()];
        app.engine.queue = loaded_queue(
            "spotify:track:a",
            &[
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:ctx1",
                "spotify:track:c",
            ],
        );
        assert_eq!(
            app.engine.queued_rows_len(),
            2,
            "the context's own copy of c does not count as queued"
        );
        app.engine.manual_queue.clear();
        assert_eq!(app.engine.queued_rows_len(), 0);
    }

    /// Changing shuffle rechecks the queue to reflect the new playback order.
    #[test]
    fn changing_shuffle_schedules_a_queue_recheck() {
        let mut app = headless_app();
        app.engine.queue = loaded_queue("spotify:track:a", &["spotify:track:b"]);
        assert!(app.engine.queue_recheck_at.is_none());
        app.engine.set_shuffle(true);
        assert!(
            app.engine.queue_recheck_at.is_some(),
            "toggling shuffle asks Spotify for the reordered queue"
        );
    }

    /// Toggling shuffle for local playback rechecks the queue, reveals the
    /// new playback order without losing hand-queued songs, and ignores an
    /// old response from before the toggle.
    #[test]
    fn shuffling_local_playback_updates_queue_and_keeps_user_songs() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.manual_queue = vec!["spotify:track:manual1".into()];
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
                "spotify:track:ctx3",
            ],
        );
        assert_eq!(app.engine.queued_rows_len(), 1);

        // Toggle shuffle on local player.
        app.engine.set_shuffle(true);
        assert!(
            app.engine.queue_recheck_at.is_some(),
            "toggling shuffle schedules a queue recheck"
        );

        // Start the queue refresh with a new sequence number.
        app.engine.refresh_queue(true);
        let seq = app.engine.queue_seq;

        // A response from before the shuffle command is dropped unread.
        let old_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
                queued_song("spotify:track:ctx3"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: seq - 1,
            result: Ok(old_response),
        });
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
                "spotify:track:ctx3",
            ],
            "the superseded pre-shuffle response is dropped unread"
        );

        // When the fresh response arrives, the context rows reflect the new shuffle
        // order while manually queued rows stay on top.
        let shuffled_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx3"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(shuffled_response),
        });

        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:playing"));
        assert_eq!(
            next,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx3",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ],
            "the user's song stays on top and the shuffled context order is shown"
        );
        assert_eq!(
            app.engine.queued_rows_len(),
            1,
            "the user's queued section is preserved"
        );
    }

    /// Toggling shuffle for remote playback rechecks the queue on confirmation,
    /// adopts the new shuffle order while keeping hand-queued songs, and drops
    /// an overtaken response.
    #[test]
    fn shuffling_remote_playback_updates_queue_and_keeps_user_songs() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local_ready = false;
        app.engine.selected_device = Some("speaker".into());
        app.engine.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                device: Some(crate::api::models::Device {
                    id: Some("speaker".into()),
                    name: "Speaker".into(),
                    is_active: true,
                    ..Default::default()
                }),
                item: Some(crate::api::models::PlayableItem::Track(
                    crate::api::models::Track {
                        uri: "spotify:track:playing".into(),
                        ..Default::default()
                    },
                )),
                is_playing: true,
                shuffle_state: false,
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        app.engine.manual_queue = vec!["spotify:track:manual1".into()];
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ],
        );
        assert_eq!(app.engine.queued_rows_len(), 1);

        // Turn on shuffle on the remote target.
        app.engine.set_shuffle(true);
        assert!(app.engine.queue_recheck_at.is_some());

        // Spotify confirms the remote shuffle action.
        app.handle_api(ApiResponse::Remote {
            action: RemoteAction::Shuffle,
            result: Ok(()),
        });
        assert!(
            app.engine.queue_recheck_at.is_some(),
            "confirmed remote shuffle rechecks the queue"
        );

        // Fetch the queue with a new sequence number.
        app.engine.refresh_queue(true);
        let seq = app.engine.queue_seq;

        // An older response is ignored.
        let stale = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: seq - 1,
            result: Ok(stale),
        });
        assert_eq!(
            queue_uris(&app).1,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ]
        );

        // The confirming response shows the new shuffle order.
        let shuffled = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx2"),
                queued_song("spotify:track:ctx1"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(shuffled),
        });

        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:playing"));
        assert_eq!(
            next,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx2",
                "spotify:track:ctx1",
            ],
            "remote shuffle preserves hand-queued songs and updates context rows"
        );
        assert_eq!(app.engine.queued_rows_len(), 1);
    }

    /// A queue fetch already in flight before a local reorder must not be
    /// allowed to land afterwards and undo it. Once resynced with the local
    /// engine, a fresh fetch still reporting the pre-drag order is likewise
    /// rejected as stale until it catches up.
    #[test]
    fn stale_queue_response_after_local_reorder_does_not_undo_it() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local_ready = true;
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.manual_queue = vec![
            "spotify:track:manual1".into(),
            "spotify:track:manual2".into(),
        ];
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:manual1",
                "spotify:track:manual2",
                "spotify:track:ctx1",
            ],
        );

        // A periodic refresh is already in flight before the drag lands.
        app.engine.refresh_queue(true);
        let outstanding_seq = app.engine.queue_seq;

        let ctx = egui::Context::default();
        app.apply(Action::MoveInQueue { from: 0, to: 2 }, &ctx);

        let reordered = vec![
            "spotify:track:manual2".to_string(),
            "spotify:track:manual1".to_string(),
            "spotify:track:ctx1".to_string(),
        ];
        assert_eq!(
            queue_uris(&app).1,
            reordered,
            "the reorder is applied optimistically"
        );

        // The request that was already outstanding lands after the move,
        // still reporting the pre-drag order. It must be superseded.
        let pre_drag_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:manual2"),
                queued_song("spotify:track:ctx1"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: outstanding_seq,
            result: Ok(pre_drag_response.clone()),
        });
        assert_eq!(
            queue_uris(&app).1,
            reordered,
            "a late pre-drag response must not undo the move"
        );

        // A fresh fetch, issued after the move, still reports the pre-drag
        // order because the local engine has not caught up to the resync
        // yet. It is rejected as stale rather than accepted.
        app.engine.queue_recheck_at = None;
        app.engine.refresh_queue(true);
        let seq = app.engine.queue_seq;
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(pre_drag_response),
        });
        assert_eq!(
            queue_uris(&app).1,
            reordered,
            "a lagging fresh response must not undo the move either"
        );
        assert_eq!(app.engine.queue_stale_retries, 1);
        assert!(app.engine.queue_recheck_at.is_some());

        // Once the resync has landed, the confirmed order is accepted.
        let resynced_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual2"),
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx1"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(resynced_response),
        });
        assert_eq!(app.engine.queue_stale_retries, 0);
        assert!(app.engine.queue_reorder_pending.is_none());
        assert_eq!(queue_uris(&app).1, reordered);
    }

    /// Moving a queued row must keep every pending addition pointing at its
    /// own song, not whichever row now sits in its old `manual_queue` slot.
    #[test]
    fn moving_a_queued_row_keeps_pending_additions_on_their_own_song() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local_ready = true;
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.manual_queue = vec![
            "spotify:track:m0".into(),
            "spotify:track:m1".into(),
            "spotify:track:m2".into(),
        ];
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &["spotify:track:m0", "spotify:track:m1", "spotify:track:m2"],
        );
        app.engine.pending_queue_adds = vec![
            PendingQueueAdd {
                item: queued_song("spotify:track:m0"),
                at: Instant::now(),
                manual_index: 0,
                write: None,
            },
            PendingQueueAdd {
                item: queued_song("spotify:track:m1"),
                at: Instant::now(),
                manual_index: 1,
                write: None,
            },
            PendingQueueAdd {
                item: queued_song("spotify:track:m2"),
                at: Instant::now(),
                manual_index: 2,
                write: None,
            },
        ];

        let ctx = egui::Context::default();
        // Drag "m0" past the end: it lands last, "m1" and "m2" each shift up one.
        app.apply(Action::MoveInQueue { from: 0, to: 3 }, &ctx);

        assert_eq!(
            app.engine.manual_queue,
            ["spotify:track:m1", "spotify:track:m2", "spotify:track:m0"]
        );
        assert_eq!(app.engine.pending_queue_adds.len(), 3);
        for addition in &app.engine.pending_queue_adds {
            assert_eq!(
                app.engine.manual_queue[addition.manual_index],
                addition.item.uri(),
                "pending addition must still name the song at its own index"
            );
        }
    }

    /// Local playback with three songs under Playing next and one context
    /// row after them, for the repeated-move tests (#598).
    fn app_with_local_manual_queue() -> App {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local_ready = true;
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.manual_queue = vec![
            "spotify:track:a".into(),
            "spotify:track:b".into(),
            "spotify:track:c".into(),
        ];
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:a",
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:ctx",
            ],
        );
        app
    }

    /// Each local move gets the full stale-answer allowance. Moves made in
    /// quick succession used to share one budget, so an answer from the
    /// middle of the engine's clear-and-re-add was accepted after a later
    /// move and dropped the queued songs from Playing next (#598).
    #[test]
    fn every_local_move_restarts_the_stale_queue_allowance() {
        let mut app = app_with_local_manual_queue();
        let ctx = egui::Context::default();
        // Mid-resync, the engine reports the context with no queued songs.
        let mid_resync = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![queued_song("spotify:track:ctx")],
        };
        let answer_stale = |app: &mut App| {
            app.engine.refresh_queue(true);
            let seq = app.engine.queue_seq;
            app.handle_api(ApiResponse::Queue {
                seq,
                result: Ok(mid_resync.clone()),
            });
        };

        app.apply(Action::MoveInQueue { from: 0, to: 2 }, &ctx);
        for _ in 0..QUEUE_STALE_RETRIES - 1 {
            answer_stale(&mut app);
        }
        app.apply(Action::MoveInQueue { from: 2, to: 0 }, &ctx);
        let moved = vec![
            "spotify:track:c".to_string(),
            "spotify:track:b".to_string(),
            "spotify:track:a".to_string(),
            "spotify:track:ctx".to_string(),
        ];
        for _ in 0..QUEUE_STALE_RETRIES - 1 {
            answer_stale(&mut app);
            assert_eq!(
                queue_uris(&app).1,
                moved,
                "a lagging answer must not undo the latest move"
            );
        }
        assert_eq!(app.engine.queued_rows_len(), 3);
    }

    /// When the queue on show disagrees with the order the engine was
    /// given, a move must still move the dragged song, and the rows must
    /// then show the order that will actually play (#598).
    #[test]
    fn moving_a_row_shows_the_order_that_will_play() {
        let mut app = app_with_local_manual_queue();
        // Spotify's answer was accepted with two queued rows swapped.
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:b",
                "spotify:track:a",
                "spotify:track:c",
                "spotify:track:ctx",
            ],
        );
        let ctx = egui::Context::default();
        // The view resolves the dragged "b" to its manual_queue index.
        app.apply(Action::MoveInQueue { from: 1, to: 3 }, &ctx);

        assert_eq!(
            app.engine.manual_queue,
            ["spotify:track:a", "spotify:track:c", "spotify:track:b"]
        );
        assert_eq!(
            queue_uris(&app).1,
            [
                "spotify:track:a",
                "spotify:track:c",
                "spotify:track:b",
                "spotify:track:ctx",
            ],
            "the rows must match the order sent to the engine"
        );
    }

    /// Remove from queue takes out the chosen row only: other copies of
    /// the song stay, and so does the context after Playing next (#721).
    #[test]
    fn removing_a_queued_row_takes_only_that_row() {
        let mut app = app_with_local_manual_queue();
        app.engine.manual_queue = vec![
            "spotify:track:a".into(),
            "spotify:track:b".into(),
            "spotify:track:a".into(),
        ];
        // Spotify's accepted answer lists the queued rows in another order.
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:a",
                "spotify:track:a",
                "spotify:track:b",
                "spotify:track:ctx",
            ],
        );
        app.engine.pending_queue_adds = vec![PendingQueueAdd {
            item: queued_song("spotify:track:a"),
            at: Instant::now(),
            manual_index: 2,
            write: None,
        }];
        let ctx = egui::Context::default();
        // A Next up row, and an index that no longer holds its song, stay.
        app.apply(
            Action::RemoveFromQueue {
                rows: vec![
                    (3, "spotify:track:ctx".into()),
                    (0, "spotify:track:b".into()),
                ],
            },
            &ctx,
        );
        assert_eq!(app.engine.manual_queue.len(), 3);
        assert_eq!(queue_uris(&app).1.len(), 4);

        app.apply(
            Action::RemoveFromQueue {
                rows: vec![(2, "spotify:track:b".into())],
            },
            &ctx,
        );
        assert_eq!(app.engine.manual_queue, ["spotify:track:a", "spotify:track:a"]);
        assert_eq!(
            queue_uris(&app).1,
            ["spotify:track:a", "spotify:track:a", "spotify:track:ctx"]
        );
        assert_eq!(app.engine.queued_rows_len(), 2);
        assert_eq!(
            app.engine.pending_queue_adds[0].manual_index, 1,
            "a pending addition must still name its own copy"
        );
    }

    /// Taking out the last queued row leaves the rest of Playing next as
    /// it was, so a lagging answer that still has the song after them
    /// must not bring it back (#721).
    #[test]
    fn stale_queue_response_after_removal_does_not_bring_the_song_back() {
        let mut app = app_with_local_manual_queue();
        let ctx = egui::Context::default();
        app.apply(
            Action::RemoveFromQueue {
                rows: vec![(2, "spotify:track:c".into())],
            },
            &ctx,
        );
        let removed = ["spotify:track:a", "spotify:track:b", "spotify:track:ctx"];
        assert_eq!(queue_uris(&app).1, removed);

        let answer = |app: &mut App, rows: &[&str]| {
            app.engine.refresh_queue(true);
            let seq = app.engine.queue_seq;
            app.handle_api(ApiResponse::Queue {
                seq,
                result: Ok(Queue {
                    currently_playing: Some(queued_song("spotify:track:playing")),
                    queue: rows.iter().map(|uri| queued_song(uri)).collect(),
                }),
            });
        };
        answer(
            &mut app,
            &[
                "spotify:track:a",
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:ctx",
            ],
        );
        assert_eq!(queue_uris(&app).1, removed, "the song must stay removed");
        assert_eq!(app.engine.queue_stale_retries, 1);

        answer(&mut app, &removed);
        assert!(app.engine.queue_removed.is_none());
        assert_eq!(queue_uris(&app).1, removed);
        assert_eq!(app.engine.queued_rows_len(), 2);
    }

    /// Another device's queue cannot be rewritten, so its rows stay rather
    /// than vanish and come back with the next answer.
    #[test]
    fn removing_from_another_devices_queue_changes_nothing() {
        let mut app = app_with_local_manual_queue();
        app.engine.local_ready = false;
        app.engine.selected_device = Some("phone".into());
        assert!(!app.engine.can_remove_from_queue(0));
        let ctx = egui::Context::default();
        app.apply(
            Action::RemoveFromQueue {
                rows: vec![(0, "spotify:track:a".into())],
            },
            &ctx,
        );
        assert_eq!(app.engine.manual_queue.len(), 3);
        assert_eq!(queue_uris(&app).1.len(), 4);
    }

    /// Inserting a dropped song into "Playing next" must keep every earlier
    /// pending addition pointing at its own song, not the newly inserted row.
    #[test]
    fn inserting_in_queue_keeps_pending_additions_on_their_own_song() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local_ready = true;
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.manual_queue = vec!["spotify:track:m0".into(), "spotify:track:m1".into()];
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &["spotify:track:m0", "spotify:track:m1"],
        );
        app.engine.pending_queue_adds = vec![
            PendingQueueAdd {
                item: queued_song("spotify:track:m0"),
                at: Instant::now(),
                manual_index: 0,
                write: None,
            },
            PendingQueueAdd {
                item: queued_song("spotify:track:m1"),
                at: Instant::now(),
                manual_index: 1,
                write: None,
            },
        ];

        let ctx = egui::Context::default();
        // Drop "new" between "m0" and "m1": "m1"'s pending entry must shift.
        app.apply(
            Action::InsertInQueue {
                items: vec![queued_song("spotify:track:new")],
                position: 1,
            },
            &ctx,
        );

        assert_eq!(
            app.engine.manual_queue,
            ["spotify:track:m0", "spotify:track:new", "spotify:track:m1"]
        );
        assert_eq!(app.engine.pending_queue_adds.len(), 3);
        for addition in &app.engine.pending_queue_adds {
            assert_eq!(
                app.engine.manual_queue[addition.manual_index],
                addition.item.uri(),
                "pending addition must still name the song at its own index"
            );
        }
    }

    /// A stale queue answer whose current track does not match the active
    /// local player is rejected rather than accepted after a shuffle toggle.
    #[test]
    fn stale_queue_after_shuffle_is_retried_and_not_accepted() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &["spotify:track:ctx1", "spotify:track:ctx2"],
        );

        app.engine.set_shuffle(true);
        app.engine.refresh_queue(true);
        let seq = app.engine.queue_seq;

        // A stale response that reports an old playing track is rejected.
        let stale_track_response = Queue {
            currently_playing: Some(queued_song("spotify:track:old_song")),
            queue: vec![
                queued_song("spotify:track:ctx2"),
                queued_song("spotify:track:ctx1"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(stale_track_response),
        });

        assert_eq!(
            queue_uris(&app).1,
            vec!["spotify:track:ctx1", "spotify:track:ctx2"],
            "the queue remains uncorrupted by the stale response"
        );
        assert_eq!(app.engine.queue_stale_retries, 1);
        assert!(app.engine.queue_recheck_at.is_some());
    }

    /// A lagging queue answer after a shuffle toggle with the same current song
    /// and sequence is rejected and retried until the new order arrives.
    #[test]
    fn lagging_queue_after_shuffle_is_retried_until_new_order_arrives() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.manual_queue = vec!["spotify:track:manual1".into()];
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
                "spotify:track:ctx3",
            ],
        );

        app.engine.set_shuffle(true);
        app.engine.refresh_queue(true);
        let seq = app.engine.queue_seq;

        // A lagging response arrives with the same currently playing track and
        // the current sequence, but the context rows are still in the old order.
        let lagging_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
                queued_song("spotify:track:ctx3"),
            ],
        };

        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(lagging_response.clone()),
        });

        // The stale pre-shuffle order is rejected, preserving the existing queue,
        // incrementing the retry counter, and scheduling a prompt recheck.
        assert_eq!(app.engine.queue_stale_retries, 1);
        assert!(app.engine.queue_recheck_at.is_some());
        assert_eq!(
            queue_uris(&app).1,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
                "spotify:track:ctx3",
            ]
        );

        // A second lagging response also retries.
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(lagging_response),
        });
        assert_eq!(app.engine.queue_stale_retries, 2);
        assert!(app.engine.queue_recheck_at.is_some());

        // Once the reordered response arrives, it is accepted and pending state clears.
        let new_order_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx3"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
            ],
        };

        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(new_order_response),
        });

        assert_eq!(app.engine.queue_stale_retries, 0);
        assert!(app.engine.queue_shuffle_pending.is_none());
        assert_eq!(
            queue_uris(&app).1,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx3",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ]
        );
    }

    /// If Spotify returns an unchanged queue order after shuffle, Spotifast
    /// retries up to the limit and then accepts the result as a bounded fallback.
    #[test]
    fn unchanged_shuffle_result_has_bounded_fallback() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue(
            "spotify:track:playing",
            &["spotify:track:ctx1", "spotify:track:ctx2"],
        );

        app.engine.set_shuffle(true);
        app.engine.refresh_queue(true);
        let seq = app.engine.queue_seq;

        let unchanged_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
            ],
        };

        // Responses with the pre-shuffle order are rejected as stale and scheduled for retry.
        for expected_retries in 1..=QUEUE_STALE_RETRIES {
            app.handle_api(ApiResponse::Queue {
                seq,
                result: Ok(unchanged_response.clone()),
            });
            assert_eq!(app.engine.queue_stale_retries, expected_retries);
            assert!(app.engine.queue_recheck_at.is_some());
        }

        // The next response exceeds the retry limit, so Spotifast accepts it.
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(unchanged_response),
        });
        assert_eq!(app.engine.queue_stale_retries, 0);
        assert!(app.engine.queue_shuffle_pending.is_none());
        assert_eq!(
            queue_uris(&app).1,
            vec!["spotify:track:ctx1", "spotify:track:ctx2"]
        );
    }

    fn picked(app: &App, page: &Page) -> Vec<usize> {
        app.engine.picked_rows(page)
            .map(|rows| rows.iter().copied().collect())
            .unwrap_or_default()
    }

    /// A plain click selects one row; clicking it again clears selection.
    #[test]
    fn a_plain_click_picks_one_row_and_a_second_lets_it_go() {
        let mut app = test_app("pick-one");
        let page = Page::LikedSongs;
        app.engine.pick_row(&page, "v", 3, RowPick::Only, 10);
        assert_eq!(picked(&app, &page), vec![3]);
        app.engine.pick_row(&page, "v", 5, RowPick::Only, 10);
        assert_eq!(picked(&app, &page), vec![5], "the first one is dropped");
        app.engine.pick_row(&page, "v", 5, RowPick::Only, 10);
        assert!(picked(&app, &page).is_empty(), "clicking it again lets go");
    }

    /// Ctrl-click toggles one row.
    #[test]
    fn ctrl_click_adds_and_removes_one_row() {
        let mut app = test_app("pick-toggle");
        let page = Page::LikedSongs;
        app.engine.pick_row(&page, "v", 1, RowPick::Only, 10);
        app.engine.pick_row(&page, "v", 4, RowPick::Toggle, 10);
        app.engine.pick_row(&page, "v", 7, RowPick::Toggle, 10);
        assert_eq!(picked(&app, &page), vec![1, 4, 7]);
        app.engine.pick_row(&page, "v", 4, RowPick::Toggle, 10);
        assert_eq!(
            picked(&app, &page),
            vec![1, 7],
            "the same click takes it out"
        );
    }

    /// Shift-click selects from the anchor without exceeding the list.
    #[test]
    fn shift_click_takes_the_run_back_to_the_anchor() {
        let mut app = test_app("pick-range");
        let page = Page::LikedSongs;
        app.engine.pick_row(&page, "v", 2, RowPick::Only, 10);
        app.engine.pick_row(&page, "v", 5, RowPick::Range, 10);
        assert_eq!(picked(&app, &page), vec![2, 3, 4, 5]);
        // Back the other way, from the same anchor.
        app.engine.pick_row(&page, "v", 0, RowPick::Range, 10);
        assert_eq!(picked(&app, &page), vec![0, 1, 2]);
        // A list that has since shrunk cannot be reached past its end.
        app.engine.pick_row(&page, "v", 9, RowPick::Range, 4);
        assert_eq!(picked(&app, &page), vec![2, 3]);
    }

    /// Shift-click without an anchor selects one row.
    #[test]
    fn shift_click_with_no_anchor_picks_one_row() {
        let mut app = test_app("pick-no-anchor");
        let page = Page::LikedSongs;
        app.engine.pick_row(&page, "v", 6, RowPick::Range, 10);
        assert_eq!(picked(&app, &page), vec![6]);
    }

    /// Selection clears when sorting, filtering, or paging changes the rows.
    #[test]
    fn the_rows_let_go_when_the_list_moves_underneath() {
        let mut app = test_app("pick-stale");
        let page = Page::LikedSongs;
        app.engine.pick_row(&page, "by-name|", 3, RowPick::Only, 10);
        app.engine.keep_picked_rows_for(&page, "by-name|");
        assert_eq!(picked(&app, &page), vec![3], "the same list keeps them");
        app.engine.keep_picked_rows_for(&page, "by-date|");
        assert!(picked(&app, &page).is_empty(), "a re-sort lets them go");
    }

    /// Selecting rows in another table replaces the current selection.
    #[test]
    fn picking_rows_on_another_page_replaces_the_first() {
        let mut app = test_app("pick-other-page");
        let liked = Page::LikedSongs;
        let album = Page::Album("a".to_string());
        app.engine.pick_row(&liked, "v", 1, RowPick::Only, 10);
        app.engine.pick_row(&album, "v", 2, RowPick::Only, 10);
        assert_eq!(picked(&app, &album), vec![2]);
        assert!(picked(&app, &liked).is_empty());
    }

    fn search_page(id: &str) -> crate::api::models::Page<Playlist> {
        crate::api::models::Page {
            items: vec![Playlist {
                id: id.into(),
                ..Playlist::default()
            }],
            ..Default::default()
        }
    }

    fn catalogue_answer() -> crate::api::models::SearchResults {
        crate::api::models::SearchResults {
            tracks: Some(crate::api::models::Page {
                items: vec![Track::default()],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn searching(app: &mut App, query: &str) -> u64 {
        app.engine.search.query = query.into();
        app.engine.run_search(query.into());
        let serial = app.engine.search.serial;
        app.handle_api(ApiResponse::SearchStarted {
            query: query.into(),
            serial,
            split: true,
        });
        serial
    }

    #[test]
    fn each_half_of_a_search_shows_as_it_arrives() {
        for playlists_first in [false, true] {
            let mut app = test_app("search-halves");
            let serial = searching(&mut app, "radiohead");
            let catalogue = ApiResponse::Search {
                query: "radiohead".into(),
                serial,
                result: Ok(catalogue_answer()),
            };
            let playlists = ApiResponse::SearchPlaylists {
                query: "radiohead".into(),
                serial,
                result: Ok(search_page("p")),
            };
            if playlists_first {
                app.handle_api(playlists);
                let shown = app.engine.search
                    .results
                    .get()
                    .expect("playlists appear before catalogue");
                assert_eq!(shown.playlists.as_ref().unwrap().items[0].id, "p");
                assert!(shown.tracks.is_none());
                assert!(app.engine.search.catalogue_pending);
                app.handle_api(catalogue);
            } else {
                app.handle_api(catalogue);
                let shown = app.engine.search.results.get().expect("results");
                assert_eq!(shown.tracks.as_ref().unwrap().items.len(), 1);
                assert!(shown.playlists.is_none());
                app.handle_api(playlists);
            }
            let shown = app.engine.search.results.get().expect("results");
            assert_eq!(shown.tracks.as_ref().unwrap().items.len(), 1);
            assert_eq!(shown.playlists.as_ref().unwrap().items[0].id, "p");
        }
    }

    #[test]
    fn playlists_from_an_older_search_never_join_a_newer_one() {
        let mut app = test_app("search-stale-playlists");
        let stale = searching(&mut app, "radiohead");
        app.handle_api(ApiResponse::SearchPlaylists {
            query: "radiohead".into(),
            serial: stale,
            result: Ok(search_page("stale")),
        });
        let serial = searching(&mut app, "portishead");
        app.handle_api(ApiResponse::Search {
            query: "portishead".into(),
            serial,
            result: Ok(catalogue_answer()),
        });
        let shown = app.engine.search.results.get().expect("results");
        assert!(shown.playlists.is_none());
    }

    #[test]
    fn failed_search_halves_never_mix_queries_or_discard_the_successful_half() {
        for playlists_fail in [false, true] {
            for failure_first in [false, true] {
                let mut app = test_app("search-half-failure");
                let old = searching(&mut app, "old");
                app.handle_api(ApiResponse::Search {
                    query: "old".into(),
                    serial: old,
                    result: Ok(catalogue_answer()),
                });
                let serial = searching(&mut app, "new");
                assert!(
                    app.engine.search.results.get().is_none(),
                    "old songs cannot label a new query"
                );
                let failure = if playlists_fail {
                    ApiResponse::SearchPlaylists {
                        query: "new".into(),
                        serial,
                        result: Err(crate::api::ApiError::RateLimited),
                    }
                } else {
                    ApiResponse::Search {
                        query: "new".into(),
                        serial,
                        result: Err(crate::api::ApiError::RateLimited),
                    }
                };
                let success = if playlists_fail {
                    ApiResponse::Search {
                        query: "new".into(),
                        serial,
                        result: Ok(catalogue_answer()),
                    }
                } else {
                    ApiResponse::SearchPlaylists {
                        query: "new".into(),
                        serial,
                        result: Ok(search_page("new")),
                    }
                };
                if failure_first {
                    app.handle_api(failure);
                    assert!(matches!(app.engine.search.results, Loadable::Loading));
                    app.handle_api(success);
                } else {
                    app.handle_api(success);
                    assert!(app.engine.search.results.get().is_some());
                    app.handle_api(failure);
                }
                let shown = app.engine.search.results.get().expect("successful half survives");
                assert_eq!(shown.tracks.is_some(), playlists_fail);
                assert_eq!(shown.playlists.is_some(), !playlists_fail);
                assert_eq!(app.engine.search.results_serial, serial);
                assert!(app.engine.search.error.is_some());
                assert!(!app.engine.search.catalogue_pending && !app.engine.search.playlists_pending);
                app.engine.backend.shutdown();
            }
        }
    }

    #[test]
    fn both_search_failures_finish_loading_and_the_same_query_can_retry() {
        let mut app = test_app("search-both-fail");
        let serial = searching(&mut app, "new");
        app.handle_api(ApiResponse::Search {
            query: "new".into(),
            serial,
            result: Err(crate::api::ApiError::RateLimited),
        });
        app.handle_api(ApiResponse::SearchPlaylists {
            query: "new".into(),
            serial,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert!(matches!(app.engine.search.results, Loadable::Failed(_)));
        assert!(!app.engine.search.catalogue_pending && !app.engine.search.playlists_pending);
        app.engine.run_search("new".into());
        assert!(app.engine.search.serial > serial);
        assert!(app.engine.search.error.is_none());
        assert!(matches!(app.engine.search.results, Loadable::Loading));
        app.engine.backend.shutdown();
    }

    #[test]
    fn clearing_or_signing_out_rejects_both_late_search_halves() {
        for sign_out in [false, true] {
            let mut app = test_app("search-cancel");
            let serial = searching(&mut app, "old");
            if sign_out {
                app.engine.reset_data();
            } else {
                app.engine.run_search(String::new());
            }
            app.handle_api(ApiResponse::Search {
                query: "old".into(),
                serial,
                result: Ok(catalogue_answer()),
            });
            app.handle_api(ApiResponse::SearchPlaylists {
                query: "old".into(),
                serial,
                result: Ok(search_page("old")),
            });
            assert!(matches!(app.engine.search.results, Loadable::NotLoaded));
            assert!(app.engine.search.playlists.is_none());
            assert!(!app.engine.search.catalogue_pending && !app.engine.search.playlists_pending);
            app.engine.backend.shutdown();
        }
    }

    fn answer_search(app: &mut App, query: &str, serial: u64) {
        app.handle_api(ApiResponse::Search {
            query: query.into(),
            serial,
            result: Ok(catalogue_answer()),
        });
    }

    /// A query the debounce committed after a pause, answered with results.
    fn typed_search(app: &mut App, query: &str) {
        app.engine.search.query = query.into();
        app.engine.run_typed_search(query.into());
        let serial = app.engine.search.serial;
        answer_search(app, query, serial);
    }

    /// The field emptied, or its selection typed over.
    fn end_search_run(app: &mut App) {
        app.engine.actions.push(Action::EndSearchRun);
        app.apply_actions(&egui::Context::default());
    }

    /// Enter in the search field.
    fn submit_search(app: &mut App, query: &str) {
        app.engine.search.query = query.into();
        app.engine.actions.push(Action::Search(query.into()));
        app.apply_actions(&egui::Context::default());
    }

    #[test]
    fn pauses_while_typing_record_only_the_final_query() {
        let mut app = test_app("search-typing-prefixes");
        app.engine.settings.search_history = vec!["older".into()];
        for query in ["ab", "abs", "absolut", "abso", "absolutely"] {
            typed_search(&mut app, query);
        }
        assert_eq!(app.engine.settings.search_history, ["absolutely", "older"]);

        // Enter on a query whose typed search is still on its way ends the
        // run, and its answer still replaces the run's prefix.
        end_search_run(&mut app);
        app.engine.settings.search_history.clear();
        typed_search(&mut app, "abs");
        app.engine.search.query = "absolutely".into();
        app.engine.run_typed_search("absolutely".into());
        let serial = app.engine.search.serial;
        submit_search(&mut app, "absolutely");
        assert_eq!(app.engine.search.serial, serial);
        answer_search(&mut app, "absolutely", serial);
        assert_eq!(app.engine.settings.search_history, ["absolutely"]);
        typed_search(&mut app, "absolutely sweet");
        assert_eq!(
            app.engine.settings.search_history,
            ["absolutely sweet", "absolutely"]
        );
        app.engine.backend.shutdown();
    }

    #[test]
    fn a_new_query_or_a_finished_search_keeps_its_own_recent_entry() {
        let mut app = test_app("search-typing-separate");
        // Typing over the whole query starts a different search, even when
        // the new query is a prefix of the old one.
        typed_search(&mut app, "beatles");
        end_search_run(&mut app);
        typed_search(&mut app, "beat");
        assert_eq!(app.engine.settings.search_history, ["beat", "beatles"]);

        // A different query typed after deleting the old one.
        typed_search(&mut app, "queen");
        assert_eq!(app.engine.settings.search_history, ["queen", "beat", "beatles"]);

        // Emptying the field ends the run.
        app.engine.settings.search_history.clear();
        typed_search(&mut app, "ab");
        end_search_run(&mut app);
        app.engine.run_typed_search(String::new());
        typed_search(&mut app, "abba");
        assert_eq!(app.engine.settings.search_history, ["abba", "ab"]);

        // A search submitted with Enter is kept even if typing extends it.
        end_search_run(&mut app);
        app.engine.settings.search_history.clear();
        submit_search(&mut app, "ab");
        let serial = app.engine.search.serial;
        answer_search(&mut app, "ab", serial);
        typed_search(&mut app, "abba");
        assert_eq!(app.engine.settings.search_history, ["abba", "ab"]);

        // So is Enter on a query that already has results.
        end_search_run(&mut app);
        app.engine.settings.search_history.clear();
        typed_search(&mut app, "ab");
        submit_search(&mut app, "ab");
        typed_search(&mut app, "abba");
        assert_eq!(app.engine.settings.search_history, ["abba", "ab"]);
        app.engine.backend.shutdown();
    }

    #[test]
    fn typing_never_replaces_a_recent_search_it_did_not_add() {
        let mut app = test_app("search-typing-existing");
        app.engine.settings.search_history = vec!["older".into(), "ab".into()];
        typed_search(&mut app, "ab");
        typed_search(&mut app, "abc");
        assert_eq!(app.engine.settings.search_history, ["abc", "ab", "older"]);

        // An answer for the run before a selection was typed over belongs
        // to that run.
        app.engine.settings.search_history.clear();
        app.engine.search.query = "beatles".into();
        app.engine.run_typed_search("beatles".into());
        let serial = app.engine.search.serial;
        end_search_run(&mut app);
        answer_search(&mut app, "beatles", serial);
        typed_search(&mut app, "beat");
        assert_eq!(app.engine.settings.search_history, ["beat", "beatles"]);
        app.engine.backend.shutdown();
    }

    fn cover_dialog(request: Option<u64>) -> Dialog {
        Dialog::EditPlaylist {
            id: "pl1".into(),
            name: "Test".into(),
            description: String::new(),
            public: Some(false),
            cover: crate::playlist_cover::Draft {
                request,
                ..Default::default()
            },
        }
    }

    fn test_cover() -> crate::playlist_cover::Cover {
        test_cover_color([220, 20, 40])
    }

    fn test_cover_color(color: [u8; 3]) -> crate::playlist_cover::Cover {
        let pixels = image::RgbImage::from_pixel(16, 16, image::Rgb(color));
        let mut bytes = std::io::Cursor::new(Vec::new());
        pixels
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::playlist_cover::prepare(bytes.get_ref()).unwrap()
    }

    #[test]
    fn reopening_cover_details_does_not_start_a_second_upload() {
        let mut app = test_app("cover-reopened-in-flight");
        app.engine.backend.set_offline(true);
        let ctx = egui::Context::default();
        app.engine.dialog = Some(cover_dialog(None));
        if let Some(Dialog::EditPlaylist { cover, .. }) = &mut app.engine.dialog {
            cover.selection = Some(test_cover());
        }
        app.apply(Action::UploadPlaylistCover("pl1".into()), &ctx);
        let request = app.engine.cover_request;
        app.apply(Action::CloseDialog, &ctx);
        app.apply(Action::ShowDialog(cover_dialog(None)), &ctx);
        // A stale action or another UI entry point cannot overlap that PUT.
        if let Some(Dialog::EditPlaylist { cover, .. }) = &mut app.engine.dialog {
            cover.selection = Some(test_cover());
        }
        app.apply(Action::UploadPlaylistCover("pl1".into()), &ctx);
        assert_eq!(
            app.engine.cover_request, request,
            "the first upload still owns the playlist"
        );
        let Some(Dialog::EditPlaylist { cover, .. }) = &app.engine.dialog else {
            panic!()
        };
        assert_eq!(cover.uploading, Some(request));
        app.engine.backend.shutdown();
    }

    #[test]
    fn cover_picker_cancellation_and_stale_completion_preserve_the_dialog() {
        let mut app = test_app("cover-picker");
        app.engine.dialog = Some(cover_dialog(Some(2)));
        app.engine.cover_chosen("pl1", 1, Ok(Some(test_cover())));
        let Some(Dialog::EditPlaylist { cover, .. }) = &app.engine.dialog else {
            panic!()
        };
        assert!(cover.selection.is_none());
        assert_eq!(cover.request, Some(2));
        app.engine.cover_chosen("pl1", 2, Ok(None));
        let Some(Dialog::EditPlaylist { cover, .. }) = &app.engine.dialog else {
            panic!()
        };
        assert!(cover.request.is_none());
        assert!(cover.error.is_none());
        app.engine.dialog = None;
        app.engine.cover_chosen("pl1", 2, Ok(Some(test_cover())));
        assert!(app.engine.dialog.is_none());
        app.engine.backend.shutdown();
    }

    #[test]
    fn failed_cover_upload_preserves_preview_and_success_refreshes_library_art() {
        let mut app = test_app("cover-upload");
        app.engine.dialog = Some(cover_dialog(None));
        let cover = test_cover();
        if let Some(Dialog::EditPlaylist { cover: draft, .. }) = &mut app.engine.dialog {
            draft.selection = Some(cover.clone());
            draft.uploading = Some(1);
        }
        app.engine.cover_uploads.insert("pl1".into(), 1);
        app.handle_api(ApiResponse::PlaylistCoverUploaded {
            id: "pl1".into(),
            request: 1,
            previous_urls: vec![],
            cover: cover.clone(),
            result: Err(crate::api::ApiError::Status {
                status: 403,
                message: "Forbidden".into(),
            }),
        });
        let Some(Dialog::EditPlaylist { cover: draft, .. }) = &app.engine.dialog else {
            panic!()
        };
        assert!(draft.uploading.is_none());
        assert!(draft.selection.is_some());
        assert!(draft.error.as_ref().unwrap().contains("sign in again"));
        app.engine.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            ..Default::default()
        }]);
        if let Some(Dialog::EditPlaylist { cover: draft, .. }) = &mut app.engine.dialog {
            draft.uploading = Some(2);
        }
        app.engine.cover_uploads.insert("pl1".into(), 2);
        app.handle_api(ApiResponse::PlaylistCoverUploaded {
            id: "pl1".into(),
            request: 2,
            previous_urls: vec![],
            cover: cover.clone(),
            result: Ok(()),
        });
        assert_eq!(
            app.engine.library.playlists.get().unwrap()[0].images[0].url,
            cover.uri
        );
        assert!(app.engine.uploaded_covers.contains_key("pl1"));
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![Playlist {
                    id: "pl1".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        });
        assert_eq!(
            app.engine.library.playlists.get().unwrap()[0].images[0].url,
            cover.uri,
            "lagging metadata must not undo the uploaded artwork"
        );
        let Some(Dialog::EditPlaylist { cover: draft, .. }) = &app.engine.dialog else {
            panic!()
        };
        assert!(draft.selection.is_none());
        assert!(draft.error.is_none());
        app.engine.backend.shutdown();
    }

    #[test]
    fn old_upload_completion_does_not_change_a_reopened_dialog() {
        for succeeds in [false, true] {
            for newer_upload in [None, Some(2)] {
                let mut app = test_app("reopened-cover-upload");
                app.engine.dialog = Some(cover_dialog(None));
                let selected = test_cover();
                if let Some(Dialog::EditPlaylist { cover, .. }) = &mut app.engine.dialog {
                    cover.selection = Some(selected.clone());
                    cover.uploading = Some(1);
                }
                app.engine.dialog = None;
                app.engine.dialog = Some(cover_dialog(Some(3)));
                if let Some(Dialog::EditPlaylist { cover, .. }) = &mut app.engine.dialog {
                    cover.selection = Some(selected.clone());
                    cover.uploading = newer_upload;
                    cover.error = Some("Newer selection error".into());
                }
                app.engine.cover_uploads
                    .insert("pl1".into(), newer_upload.unwrap_or(1));
                app.handle_api(ApiResponse::PlaylistCoverUploaded {
                    id: "pl1".into(),
                    request: 1,
                    previous_urls: vec![],
                    cover: selected.clone(),
                    result: if succeeds {
                        Ok(())
                    } else {
                        Err(crate::api::ApiError::Status {
                            status: 403,
                            message: "Forbidden".into(),
                        })
                    },
                });
                let Some(Dialog::EditPlaylist { cover, .. }) = &app.engine.dialog else {
                    panic!()
                };
                assert_eq!(cover.selection.as_ref().unwrap().uri, selected.uri);
                assert_eq!(cover.uploading, newer_upload);
                assert_eq!(cover.request, Some(3));
                assert_eq!(cover.error.as_deref(), Some("Newer selection error"));
                app.engine.backend.shutdown();
            }
        }
    }

    #[test]
    fn a_late_url_from_an_earlier_upload_cannot_replace_the_new_cover() {
        let mut app = test_app("cover-lagging-earlier-upload");
        app.engine.backend.set_offline(true);
        app.engine.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            images: vec![Image {
                url: "original".into(),
                ..Default::default()
            }],
            ..Default::default()
        }]);
        let current = test_cover_color([20, 40, 220]);
        for request in [1, 2] {
            app.engine.cover_uploads.insert("pl1".into(), request);
            app.handle_api(ApiResponse::PlaylistCoverUploaded {
                id: "pl1".into(),
                request,
                previous_urls: vec!["original".into()],
                cover: if request == 1 {
                    test_cover()
                } else {
                    current.clone()
                },
                result: Ok(()),
            });
        }
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.engine.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![Playlist {
                    id: "pl1".into(),
                    images: vec![Image {
                        url: "https://art.example/earlier-upload.jpg".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }),
        });
        assert_eq!(
            app.engine.library.playlists.get().unwrap()[0].images[0].url,
            current.uri,
            "a new URL does not establish which upload it contains"
        );
        let earlier = vec![Image {
            url: "https://art.example/earlier-upload.jpg".into(),
            ..Default::default()
        }];
        // A check started for the superseded upload cannot confirm request 2.
        app.engine.cover_checked("pl1", 1, earlier.clone(), Ok(true));
        assert_eq!(app.engine.uploaded_covers["pl1"].request, 2);
        let latest = vec![Image {
            url: "https://art.example/current-upload.jpg".into(),
            ..Default::default()
        }];
        let mut latest_reply = latest.clone();
        app.engine.reconcile_playlist_cover("pl1", &mut latest_reply);
        assert_eq!(latest_reply[0].url, current.uri);
        assert_eq!(app.engine.uploaded_covers["pl1"].checking, Some(earlier.clone()));
        assert_eq!(app.engine.uploaded_covers["pl1"].next_images, Some(latest.clone()));
        app.engine.cover_checked("pl1", 2, earlier, Ok(false));
        assert_eq!(app.engine.uploaded_covers["pl1"].checking, Some(latest.clone()));
        assert_eq!(
            app.engine.library.playlists.get().unwrap()[0].images[0].url,
            current.uri
        );
        // A failed download also preserves the preview and remains retryable.
        app.engine.cover_checked("pl1", 2, latest.clone(), Err("offline".into()));
        assert!(app.engine.uploaded_covers["pl1"].checking.is_none());
        app.engine.reconcile_playlist_cover("pl1", &mut latest.clone());
        app.engine.cover_checked("pl1", 2, latest.clone(), Ok(true));
        assert!(!app.engine.uploaded_covers.contains_key("pl1"));
        assert_eq!(app.engine.library.playlists.get().unwrap()[0].images, latest);
        app.engine.backend.shutdown();
    }

    #[test]
    fn confirmed_spotify_art_retires_the_override_and_accepts_later_changes() {
        for confirm_from_library in [false, true] {
            let mut app = test_app("confirmed-cover");
            let remote = |url: &str| {
                vec![crate::api::models::Image {
                    url: url.into(),
                    width: None,
                    height: None,
                }]
            };
            let playlist = |url: &str| Playlist {
                id: "pl1".into(),
                images: remote(url),
                ..Default::default()
            };
            app.engine.library.playlists = Loadable::Loaded(vec![playlist("old")]);
            let page = app.engine.playlist_pages.entry("pl1".into()).or_default();
            page.playlist = Loadable::Loaded(playlist("old"));
            let generation = page.generation;
            let cover = test_cover();
            app.engine.cover_uploads.insert("pl1".into(), 1);
            app.handle_api(ApiResponse::PlaylistCoverUploaded {
                id: "pl1".into(),
                request: 1,
                previous_urls: vec!["old".into()],
                cover: cover.clone(),
                result: Ok(()),
            });
            app.handle_api(ApiResponse::Playlist {
                id: "pl1".into(),
                generation: generation + 1,
                result: Ok(playlist("stale-generation")),
            });
            assert!(app.engine.uploaded_covers.contains_key("pl1"));
            for url in ["old", "confirmed", "changed-elsewhere"] {
                if confirm_from_library {
                    app.handle_api(ApiResponse::MyPlaylists {
                        offset: 0,
                        generation: app.engine.library.playlists_generation,
                        result: Ok(crate::api::models::Page {
                            items: vec![playlist(url)],
                            ..Default::default()
                        }),
                    });
                } else {
                    app.handle_api(ApiResponse::Playlist {
                        id: "pl1".into(),
                        generation,
                        result: Ok(playlist(url)),
                    });
                }
                if url == "confirmed" {
                    assert_eq!(
                        app.engine.library.playlists.get().unwrap()[0].images[0].url,
                        cover.uri
                    );
                    app.engine.cover_checked("pl1", 1, remote(url), Ok(true));
                }
                let expected = if url == "old" { &cover.uri } else { url };
                if confirm_from_library {
                    assert_eq!(
                        app.engine.library.playlists.get().unwrap()[0].images[0].url,
                        expected
                    );
                } else {
                    assert_eq!(
                        app.engine.playlist_pages["pl1"].playlist.get().unwrap().images[0].url,
                        expected
                    );
                }
                assert_eq!(app.engine.uploaded_covers.contains_key("pl1"), url == "old");
                if url == "confirmed" {
                    assert_eq!(app.engine.library.playlists.get().unwrap()[0].images[0].url, url);
                    assert_eq!(
                        app.engine.playlist_pages["pl1"].playlist.get().unwrap().images[0].url,
                        url
                    );
                }
            }
            app.engine.backend.shutdown();
        }
    }

    fn test_app(name: &str) -> App {
        let root =
            std::env::temp_dir().join(format!("spotifast-{name}-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut app = App::new(
            &Waker::default(),
            AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.reveal_theme_changes = false;
        app
    }

    /// Demo mode is offline, but it reads cover art and its colours like
    /// the real app: the page header and the player bar take their tint.
    #[test]
    fn an_offline_app_keeps_the_colours_read_from_cover_art() {
        let mut app = test_app("offline-accent");
        app.engine.offline = true;
        // The colour arrives as the backend reports it; nothing is fetched.
        let url = "https://i.scdn.co/image/offline-accent";
        app.handle_backend_events(vec![Event::Accent {
            url: url.into(),
            color: [200, 40, 40],
        }]);
        assert_eq!(
            app.engine.tint_for(Some(url)),
            Some([200, 40, 40])
        );
    }

    /// Two lookups of one adder can overlap; the one that found nothing
    /// must not turn a shown name back into the bare id.
    #[test]
    fn a_failed_name_lookup_keeps_the_name_already_found() {
        let mut app = test_app("adder-name-race");
        app.engine.request_user_names(vec!["kasia-id".into()]);
        app.engine.set_user_name("kasia-id".into(), Some("Kasia".into()));
        let revision = app.engine.user_names_revision;
        app.engine.set_user_name("kasia-id".into(), None);
        assert_eq!(
            app.engine.user_names.get("kasia-id"),
            Some(&Some("Kasia".to_string()))
        );
        assert_eq!(app.engine.user_names_revision, revision, "nothing to redraw");
        // A renamed account still shows its new name.
        app.engine.set_user_name("kasia-id".into(), Some("Kasia M.".into()));
        assert_eq!(
            app.engine.user_names.get("kasia-id"),
            Some(&Some("Kasia M.".to_string()))
        );
        // An id nobody could name stays unnamed, to be shown as the id.
        app.engine.set_user_name("nobody".into(), None);
        assert_eq!(app.engine.user_names.get("nobody"), Some(&None));
        app.engine.backend.shutdown();
    }

    #[test]
    fn prerelease_answers_cannot_replace_newer_links_or_manual_navigation() {
        let mut app = test_app("prerelease-stale");
        let uri = "spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL";
        let data = || {
            crate::prerelease::Prerelease::decode(include_bytes!("testdata/prerelease.pb"), uri)
                .unwrap()
        };
        app.engine.user = Some(crate::api::models::User::default());
        app.engine.pending_link = Some(uri.into());
        app.engine.prerelease_request = Some(2);
        app.engine.receive_prerelease(uri, 1, Ok(data()));
        assert!(app.engine.album_pages.is_empty());
        assert_eq!(app.engine.pending_link.as_deref(), Some(uri));
        app.engine.open(Page::Home);
        app.engine.receive_prerelease(uri, 2, Ok(data()));
        assert!(
            app.engine.album_pages.is_empty(),
            "manual navigation cancels the external link"
        );
        app.engine.pending_link = Some(uri.into());
        app.engine.prerelease_request = Some(3);
        app.engine.receive_prerelease(uri, 3, Ok(data()));
        assert_eq!(app.engine.page(), &Page::Album("5BYKG4MHfySZthe2W6r7n4".into()));
        assert!(app.engine.pending_link.is_none());
        assert!(
            app.engine.album_pages["5BYKG4MHfySZthe2W6r7n4"]
                .prerelease
                .is_some()
        );
    }

    #[test]
    fn uncertain_presave_does_not_claim_the_library_changed_or_replay() {
        let mut app = test_app("prerelease-presave");
        let uri = "spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL";
        let mut data =
            crate::prerelease::Prerelease::decode(include_bytes!("testdata/prerelease.pb"), uri)
                .unwrap();
        data.saved = Some(true);
        data.saving = true;
        app.engine.album_pages.insert(
            data.album.id.clone(),
            AlbumPage {
                generation: 7,
                prerelease: Some(data),
                ..Default::default()
            },
        );
        app.engine.receive_prerelease_saved(
            "spotify:album:5BYKG4MHfySZthe2W6r7n4",
            true,
            6,
            Err("stale".into()),
        );
        assert!(
            app.engine.album_pages["5BYKG4MHfySZthe2W6r7n4"]
                .prerelease
                .as_ref()
                .unwrap()
                .saving
        );
        app.engine.receive_prerelease_saved(
            "spotify:album:5BYKG4MHfySZthe2W6r7n4",
            true,
            7,
            Err("confirmation timed out".into()),
        );
        let data = app.engine.album_pages["5BYKG4MHfySZthe2W6r7n4"]
            .prerelease
            .as_ref()
            .unwrap();
        assert_eq!(data.saved, None);
        assert!(!data.saving);
        assert_eq!(data.save_error.as_deref(), Some("confirmation timed out"));
    }

    #[test]
    fn history_and_logout_cancel_pending_prerelease_navigation() {
        let mut app = test_app("prerelease-history");
        let uri = "spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL";
        app.engine.user = Some(crate::api::models::User::default());
        app.engine.open(Page::Search);
        let ctx = egui::Context::default();
        for (action, generation) in [(Action::Back, 1), (Action::Forward, 2)] {
            app.engine.pending_link = Some(uri.into());
            app.engine.prerelease_request = Some(generation);
            app.apply(action, &ctx);
            let page = app.engine.page().clone();
            let data = crate::prerelease::Prerelease::decode(
                include_bytes!("testdata/prerelease.pb"),
                uri,
            )
            .unwrap();
            app.engine.receive_prerelease(uri, generation, Ok(data));
            assert_eq!(app.engine.page(), &page);
            assert!(app.engine.album_pages.is_empty());
        }
        app.engine.pending_link = Some(uri.into());
        app.engine.prerelease_request = Some(3);
        app.engine.reset_data();
        let data =
            crate::prerelease::Prerelease::decode(include_bytes!("testdata/prerelease.pb"), uri)
                .unwrap();
        app.engine.receive_prerelease(uri, 3, Ok(data));
        assert!(app.engine.album_pages.is_empty());
    }

    #[test]
    fn cold_prerelease_link_waits_for_backend_playback_restore_once() {
        let mut app = test_app("prerelease-cold-start");
        let uri = "spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL";
        app.engine.user = Some(crate::api::models::User::default());
        app.engine.local_playback = LocalPlayback::Unavailable;
        app.engine.pending_link = Some(uri.into());
        app.engine.open_pending_link();
        assert_eq!(app.engine.pending_link.as_deref(), Some(uri));
        let request = app.engine.prerelease_request;
        assert!(request.is_some());
        app.engine.open_pending_link();
        assert_eq!(app.engine.prerelease_request, request);
        app.engine.handle_playback(LocalPlayback::Ready {
            device_id: "local".into(),
        });
        app.engine.open_pending_link();
        assert_eq!(
            app.engine.prerelease_request, request,
            "readiness must not duplicate the queued request"
        );
    }

    #[test]
    fn generic_album_playback_excludes_unreleased_preview_rows() {
        let mut app = test_app("prerelease-playable-context");
        let uri = "spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL";
        let mut data =
            crate::prerelease::Prerelease::decode(include_bytes!("testdata/prerelease.pb"), uri)
                .unwrap();
        let body = serde_json::from_str(include_str!("testdata/prerelease-album.json")).unwrap();
        let (tracks, _) = data.album_preview(&body).unwrap();
        data.tracks = tracks;
        let expected: Vec<_> = data
            .tracks
            .iter()
            .flatten()
            .filter(|track| track.is_playable == Some(true))
            .map(|track| track.uri.clone())
            .collect();
        app.engine.user = Some(crate::api::models::User::default());
        app.engine.pending_link = Some(uri.into());
        app.engine.prerelease_request = Some(1);
        let album_uri = data.album.uri.clone();
        app.engine.receive_prerelease(uri, 1, Ok(data));
        assert_eq!(app.engine.context_track_uris(&album_uri), Some(expected.clone()));
        assert!(
            app.engine.album_pages["5BYKG4MHfySZthe2W6r7n4"]
                .prerelease
                .as_ref()
                .unwrap()
                .tracks
                .len()
                > expected.len()
        );
        app.handle_api(ApiResponse::Album {
            id: "5BYKG4MHfySZthe2W6r7n4".into(),
            result: Ok(crate::api::models::Album {
                tracks: Some(crate::api::models::Page {
                    items: vec![crate::api::models::Track {
                        uri: "spotify:track:unavailable".into(),
                        is_playable: Some(false),
                        ..Default::default()
                    }],
                    total: 1,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        });
        assert_eq!(
            app.engine.context_track_uris(&album_uri),
            Some(expected),
            "a late generic header cannot restore hidden playback rows"
        );
    }

    /// The avatar a lookup found is kept like the name: a later lookup of
    /// the same id that found nothing must not take the picture away, and
    /// a new picture replaces an old one.
    #[test]
    fn an_avatar_lookup_keeps_the_picture_already_found() {
        let mut app = test_app("adder-avatar-race");
        app.engine.set_user_image("kasia-id".into(), Some("https://i.scdn.co/image/a".into()));
        app.engine.set_user_image("kasia-id".into(), None);
        assert_eq!(
            app.engine.user_images.get("kasia-id").and_then(Option::as_deref),
            Some("https://i.scdn.co/image/a")
        );
        // A renamed account's new picture shows instead.
        app.engine.set_user_image("kasia-id".into(), Some("https://i.scdn.co/image/b".into()));
        assert_eq!(
            app.engine.user_images.get("kasia-id").and_then(Option::as_deref),
            Some("https://i.scdn.co/image/b")
        );
        // An id with no picture stays without one.
        app.engine.set_user_image("nobody".into(), None);
        assert_eq!(app.engine.user_images.get("nobody"), Some(&None));
        app.engine.backend.shutdown();
    }

    /// With Random on, each switch to the mini player shows a skin other
    /// than the last one, and choosing a skin turns Random off.
    #[test]
    fn a_random_skin_changes_each_time_the_mini_player_opens() {
        let ctx = egui::Context::default();
        let mut app = test_app("random-skin");
        let skins = app.engine.dirs.skins_dir();
        std::fs::create_dir_all(&skins).unwrap();
        for name in ["A.wsz", "B.wsz"] {
            std::fs::write(skins.join(name), b"skin").unwrap();
        }
        app.apply(Action::SetRandomSkin(true), &ctx);
        for _ in 0..6 {
            let before = app.engine.settings.skin.clone();
            app.engine.settings.winamp_window = false;
            app.switch_intent = false;
            app.apply(Action::ToggleWinampWindow, &ctx);
            assert!(app.engine.settings.winamp_window);
            assert_ne!(app.engine.settings.skin, before, "never the same twice in a row");
        }
        app.apply(Action::SetSkin(Some("B.wsz".into())), &ctx);
        assert!(!app.engine.settings.random_skin);
        app.engine.settings.winamp_window = false;
        app.apply(Action::ToggleWinampWindow, &ctx);
        assert_eq!(
            app.engine.settings.skin.as_deref(),
            Some("B.wsz"),
            "a chosen skin stays"
        );
    }

    /// A change of colours keeps the old ones until the window's picture of
    /// them arrives, or a short wait passes without one, then applies.
    #[test]
    fn a_colour_change_waits_for_the_picture_of_the_old_colours() {
        let ctx = egui::Context::default();
        let mut app = test_app("theme-reveal");
        app.reveal_theme_changes = true;
        let frame = |app: &mut App, time: f64| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |ui| app.apply_theme(ui.ctx()),
            );
            output.textures_delta.clear();
            output
        };
        ctx.set_theme(egui::ThemePreference::Dark);
        frame(&mut app, 0.0);
        assert_eq!(app.palette, Palette::dark(), "the first colours at once");

        ctx.set_theme(egui::ThemePreference::Light);
        let output = frame(&mut app, 0.1);
        assert_eq!(app.palette, Palette::dark(), "held for the picture");
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Screenshot(_)))
        );
        frame(&mut app, 0.2);
        assert_eq!(app.palette, Palette::dark());
        frame(&mut app, 0.5);
        assert_eq!(app.palette, Palette::light(), "no picture came: apply");
    }

    #[test]
    fn known_albums_include_liked_and_playlist_tracks() {
        let mut app = test_app("known-track-albums");
        let album = |id: &str| Album {
            id: id.to_string(),
            ..Default::default()
        };
        app.engine.library.liked.items.push(SavedTrack {
            track: Track {
                album: Some(album("liked-album")),
                ..Default::default()
            },
            ..Default::default()
        });
        app.engine.playlist_pages.insert(
            "playlist".to_string(),
            PlaylistPage {
                items: PagedList {
                    items: vec![PlaylistItem {
                        item: Some(PlayableItem::Track(Track {
                            album: Some(album("playlist-album")),
                            ..Default::default()
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        assert_eq!(app.engine.known_album("liked-album").unwrap().id, "liked-album");
        assert_eq!(
            app.engine.known_album("playlist-album").unwrap().id,
            "playlist-album"
        );
        app.engine.backend.shutdown();
    }

    #[test]
    fn known_shows_include_saved_and_search_episodes() {
        let mut app = test_app("known-episode-shows");
        let show = |id: &str| Show {
            id: id.to_string(),
            ..Default::default()
        };
        app.engine.library.episodes.items.push(SavedEpisode {
            episode: Episode {
                show: Some(show("saved-show")),
                ..Default::default()
            },
            ..Default::default()
        });
        app.engine.search.results = Loadable::Loaded(SearchResults {
            episodes: Some(ApiPage {
                items: vec![Episode {
                    show: Some(show("search-show")),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(app.engine.known_show("saved-show").unwrap().id, "saved-show");
        assert_eq!(app.engine.known_show("search-show").unwrap().id, "search-show");
        app.engine.backend.shutdown();
    }

    #[test]
    fn custom_theme_cache_preserves_first_frame_and_settings_after_file_removal_or_corruption() {
        let mut app = test_app("custom-theme-restart");
        app.engine.backend.shutdown();
        let ctx = egui::Context::default();
        let directory = app.engine.dirs.config.join("themes");
        let file = directory.join("local.json");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            &file,
            br##"{"base":"light","colors":{"accent":"#8c3fa5"}}"##,
        )
        .unwrap();
        app.engine.settings.theme = ThemeChoice::Dark;
        app.engine.settings.audio_cache_mb = 777;
        app.engine.settings.custom_theme = Some("local.json".into());
        assert!(
            !app.custom_themes.loading(),
            "construction never starts theme discovery"
        );
        app.load_custom_themes(&Waker::default());
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        let mut expected = Palette::light();
        expected.accent = egui::Color32::from_rgb(140, 63, 165);
        assert_eq!(app.palette, expected);
        app.save_settings();
        app.engine.flush_files();
        let accepted = Settings::load(&app.engine.dirs.settings_file());
        assert!(accepted.custom_theme_cache.is_some());

        for contents in [
            None,
            Some("{broken"),
            Some(r##"{"colors":{"accent":"#bad"}}"##),
        ] {
            if let Some(contents) = contents {
                std::fs::write(&file, contents).unwrap();
            } else {
                std::fs::remove_file(&file).unwrap();
            }
            let mut restored = App::new(
                &Waker::default(),
                app.engine.dirs.clone(),
                Settings::load(&app.engine.dirs.settings_file()),
                AppOptions {
                    media_controls: false,
                    restore_sign_in: false,
                    tray: false,
                },
            );
            restored.engine.backend.shutdown();
            assert_eq!(
                restored.palette, expected,
                "even the first window clear uses the accepted palette"
            );
            let ctx = egui::Context::default();
            restored.attach(&ctx);
            restored.apply_theme(&ctx);
            assert_eq!(ctx.theme(), egui::Theme::Light);
            assert_eq!(restored.palette, expected);
            restored.load_custom_themes(&Waker::default());
            wait_for_custom_themes(&mut restored, &ctx);
            restored.apply_theme(&ctx);
            assert_eq!(restored.palette, expected);
            assert!(
                theme::catalog_detail(
                    &restored.custom_themes,
                    crate::i18n::Locale::English,
                    Some("local.json")
                )
                .contains("last usable")
            );
            restored.save_settings();
            restored.engine.flush_files();
            assert_eq!(Settings::load(&restored.engine.dirs.settings_file()), accepted);
        }
        std::fs::remove_dir_all(app.engine.dirs.config.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_chosen_language_applies_at_once_and_survives_a_restart() {
        use crate::i18n::Locale;
        use crate::settings::LanguageChoice;
        let mut app = test_app("language-choice");
        app.engine.backend.shutdown();
        let ctx = egui::Context::default();
        assert_eq!(app.engine.locale, Locale::English);
        let choice = LanguageChoice::Locale(Locale::PortugueseBrazil);
        app.apply(Action::SetLanguage(choice), &ctx);
        assert_eq!(app.engine.locale, Locale::PortugueseBrazil);
        assert!(app.engine.settings_dirty);
        app.save_settings();
        app.engine.flush_files();
        let saved = Settings::load(&app.engine.dirs.settings_file());
        assert_eq!(saved.language, choice);
        let mut restarted = App::new(
            &Waker::default(),
            app.engine.dirs.clone(),
            saved,
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        restarted.engine.backend.shutdown();
        assert_eq!(restarted.engine.locale, Locale::PortugueseBrazil);
        std::fs::remove_dir_all(app.engine.dirs.config.parent().unwrap()).unwrap();
    }

    /// What a scan reports when Omarchy is (or is not) followed.
    fn show_system_theme(
        app: &mut App,
        ctx: &egui::Context,
        theme: Option<theme::CustomTheme>,
        follows: bool,
    ) {
        app.custom_themes = theme::Catalog::preview(theme.into_iter().collect(), follows);
        app.adopt_custom_themes(ctx);
    }

    /// Closing hides to the tray only when the tray can take the app, and
    /// never overrides a quit or a switch between windows. The decision runs
    /// in the logic pass, so it also holds for a minimised window, whose
    /// drawing eframe skips.
    #[test]
    fn a_close_request_hides_to_the_tray_from_the_logic_pass() {
        use fastframe_shell::{Closed, Resident};
        let mut app = headless_app();
        app.note_close_request(true, false);
        assert_eq!(app.closed(), Closed::Quit, "no tray to hide to");
        app.note_close_request(false, true);
        assert_eq!(app.closed(), Closed::Quit, "nothing asked to close");
        app.note_close_request(true, true);
        assert_eq!(app.closed(), Closed::Hide);

        let mut app = headless_app();
        app.engine.quit_requested = true;
        app.note_close_request(true, true);
        assert_eq!(app.closed(), Closed::Quit, "Quit from the tray wins");
    }

    /// In the tray the app sleeps as long as nothing is due: until the next
    /// progress update while music plays, and a short tick while a play is
    /// pending or MilkDrop's window is open.
    #[test]
    fn the_tray_sleeps_until_something_is_due() {
        use fastframe_shell::Headless;
        let mut app = headless_app();
        assert_eq!(app.headless_wait(), Headless::WaitFor(HEADLESS_SLEEP));

        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        assert_eq!(
            app.headless_wait(),
            Headless::WaitFor(Duration::from_millis(250))
        );

        app.engine.settings.milkdrop_open = true;
        assert_eq!(app.headless_wait(), Headless::Wait);
    }

    /// Quit wins over everything, a switch between the main window and the
    /// mini player reopens at once, and closing to the tray runs headless
    /// until Show or Quit.
    #[test]
    fn the_shell_learns_what_a_closed_window_means() {
        use fastframe_shell::{Closed, Headless, Resident};
        let mut app = headless_app();
        let ctx = egui::Context::default();
        assert_eq!(app.closed(), Closed::Quit);
        app.hide_intent = true;
        assert_eq!(app.closed(), Closed::Hide);
        app.switch_intent = true;
        assert_eq!(app.closed(), Closed::Reopen);
        app.engine.quit_requested = true;
        assert_eq!(app.closed(), Closed::Quit);
        app.engine.quit_requested = false;
        Resident::window_gone(&mut app);
        assert!(app.window_hidden && !app.hide_intent);
        assert_eq!(
            app.headless_frame(&ctx),
            Headless::WaitFor(HEADLESS_SLEEP),
            "nothing due: a long sleep that a wake cuts short"
        );
        app.wants_show = true;
        assert_eq!(app.headless_frame(&ctx), Headless::Show);
        app.engine.quit_requested = true;
        assert_eq!(app.headless_frame(&ctx), Headless::Quit);
    }

    fn wait_for_custom_themes(app: &mut App, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.custom_themes.loading() {
            app.poll_custom_themes(ctx);
            assert!(Instant::now() < deadline, "theme worker did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn following_omarchy_is_automatic_live_and_never_overrides_an_explicit_choice() {
        let mut app = test_app("system-omarchy");
        app.engine.backend.shutdown();
        let ctx = egui::Context::default();
        app.engine.settings = Settings::default();
        app.window_hidden = true;
        app.engine.resume_track = Some("spotify:track:playing".into());
        app.engine.resume_position_ms = 123_000;
        let theme = theme::CustomTheme {
            filename: "omarchy.json".into(),
            palette: Palette::light(),
        };
        show_system_theme(&mut app, &ctx, Some(theme.clone()), true);
        app.apply_theme(&ctx);
        assert_eq!(app.palette, theme.palette);
        assert_eq!(app.engine.settings.theme, ThemeChoice::System);
        assert!(app.engine.settings.custom_theme.is_none());
        app.save_settings();
        app.engine.flush_files();
        let saved = Settings::load(&app.engine.dirs.settings_file());
        assert_eq!(saved.cached_palette(), Some(theme.palette));

        show_system_theme(&mut app, &ctx, None, true);
        app.apply_theme(&ctx);
        assert_eq!(
            app.palette, theme.palette,
            "missing colours retain the last palette"
        );
        for (choice, expected) in [
            (ThemeChoice::Dark, Palette::dark()),
            (ThemeChoice::Light, Palette::light()),
        ] {
            app.apply(Action::SetTheme(choice), &ctx);
            let mut updated = theme.clone();
            updated.palette.accent = egui::Color32::RED;
            show_system_theme(&mut app, &ctx, Some(updated), true);
            app.apply_theme(&ctx);
            assert_eq!(app.palette, expected);
        }
        app.apply(Action::SetTheme(ThemeChoice::System), &ctx);
        assert_eq!(app.palette.accent, egui::Color32::RED);
        let custom = theme::CustomTheme {
            filename: "mine.json".into(),
            palette: Palette::dark(),
        };
        app.custom_themes = theme::Catalog::preview(vec![custom.clone()], false);
        app.apply(Action::SetCustomTheme(custom.filename.clone()), &ctx);
        show_system_theme(&mut app, &ctx, Some(theme), true);
        app.apply_theme(&ctx);
        assert_eq!(app.palette, custom.palette);
        app.apply(Action::SetTheme(ThemeChoice::System), &ctx);
        show_system_theme(&mut app, &ctx, None, false);
        assert!(app.engine.settings.system_theme_cache.is_none());
        assert_eq!(app.theme_preference(), egui::ThemePreference::System);
        assert!(app.window_hidden);
        assert_eq!(app.engine.resume_track.as_deref(), Some("spotify:track:playing"));
        assert_eq!(app.engine.resume_position_ms, 123_000);
        std::fs::remove_dir_all(app.engine.dirs.config.parent().unwrap()).unwrap();
    }

    /// winit reports no system theme on Linux, so "Follow system" takes
    /// the desktop portal's preference, and a fixed choice ignores it.
    #[cfg(target_os = "linux")]
    #[test]
    fn follow_system_uses_the_desktops_preference_on_linux() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.engine.settings.theme = ThemeChoice::System;
        ctx.set_theme(app.theme_preference());
        app.system_appearance = Some(crate::appearance::SystemAppearance::fixed(false));
        app.apply_theme(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        assert_eq!(app.palette, Palette::light());

        app.system_appearance = Some(crate::appearance::SystemAppearance::fixed(true));
        app.apply_theme(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Dark);

        app.engine.settings.theme = ThemeChoice::Dark;
        ctx.set_theme(app.theme_preference());
        app.system_appearance = Some(crate::appearance::SystemAppearance::fixed(false));
        app.apply_theme(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Dark, "a chosen theme wins");
    }

    #[test]
    fn custom_theme_reload_updates_the_selected_palette_without_showing_the_window() {
        let mut app = test_app("custom-theme-reload");
        app.engine.backend.shutdown();
        let ctx = egui::Context::default();
        let directory = app.engine.dirs.config.join("themes");
        let file = directory.join("omarchy.json");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&file, br#"{"base":"dark"}"#).unwrap();
        app.engine.settings.custom_theme = Some("omarchy.json".into());
        app.load_custom_themes(&Waker::default());
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        assert_eq!(app.palette, Palette::dark());

        app.window_hidden = true;
        app.engine.settings.volume = 37;
        app.engine.resume_track = Some("spotify:track:playing".into());
        app.engine.resume_position_ms = 123_000;
        let queue: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> = Default::default();
        app.control_commands = Some(queue.clone());
        for (contents, expected) in [
            (r#"{"base":"light"}"#, Palette::light()),
            ("broken", Palette::light()),
            (r#"{"base":"dark"}"#, Palette::dark()),
        ] {
            std::fs::write(&file, contents).unwrap();
            queue.lock().unwrap().push(ControlCommand::ReloadThemes);
            app.handle_control_commands();
            assert!(matches!(app.engine.actions.as_slice(), [Action::ReloadThemes]));
            let action = app.engine.actions.pop().unwrap();
            app.apply(action, &ctx);
            wait_for_custom_themes(&mut app, &ctx);
            app.apply_theme(&ctx);
            assert_eq!(app.palette, expected);
            assert!(app.window_hidden);
            assert_eq!(app.engine.settings.volume, 37);
            assert_eq!(app.engine.resume_track.as_deref(), Some("spotify:track:playing"));
            assert_eq!(app.engine.resume_position_ms, 123_000);
            assert_eq!(app.engine.settings.custom_theme.as_deref(), Some("omarchy.json"));
        }

        app.apply(Action::SetTheme(ThemeChoice::Light), &ctx);
        app.apply(Action::ReloadThemes, &ctx);
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        assert_eq!(
            app.palette,
            Palette::light(),
            "a hook never selects a custom theme"
        );
        assert!(app.engine.settings.custom_theme.is_none());
        std::fs::remove_dir_all(app.engine.dirs.config.parent().unwrap()).unwrap();
    }

    #[test]
    fn custom_theme_worker_does_not_block_selection_or_restore_a_stale_choice() {
        let mut app = test_app("custom-theme-worker");
        app.engine.backend.shutdown();
        let ctx = egui::Context::default();
        let directory = app.engine.dirs.config.join("themes");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("old.json"), br#"{"base":"dark"}"#).unwrap();
        app.engine.settings.custom_theme = Some("old.json".into());
        app.load_custom_themes(&Waker::default());
        app.apply(Action::SetTheme(ThemeChoice::Light), &ctx);
        assert_eq!(app.palette, Palette::light());
        assert!(app.custom_themes.loading());
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        assert_eq!(app.palette, Palette::light());
        assert!(app.custom_themes.find("old.json").is_some());
        assert!(app.engine.settings.custom_theme.is_none());
        assert!(app.engine.settings.custom_theme_cache.is_none());
        std::fs::remove_dir_all(app.engine.dirs.config.parent().unwrap()).unwrap();
    }

    #[test]
    fn custom_theme_changes_egui_base_and_colors_together_and_builtin_choices_remain_available() {
        use crate::settings::ThemeChoiceExt;
        let mut app = test_app("custom-theme");
        let ctx = egui::Context::default();
        app.engine.settings.theme = ThemeChoice::System;
        let mut palette = Palette::light();
        palette.accent = egui::Color32::RED;
        app.custom_themes = theme::Catalog::preview(
            vec![theme::CustomTheme {
                filename: "local.json".into(),
                palette,
            }],
            false,
        );
        app.apply(Action::SetCustomTheme("local.json".into()), &ctx);
        assert_eq!(app.palette, palette);
        assert_eq!(
            app.engine.settings.theme,
            ThemeChoice::System,
            "custom selection preserves the built-in choice"
        );
        assert_eq!(ctx.theme(), egui::Theme::Light);
        assert!(!ctx.global_style().visuals.dark_mode);
        assert_eq!(ctx.global_style().visuals.panel_fill, palette.panel);
        let accepted = app.engine.settings.clone();
        app.apply(Action::SetCustomTheme("missing.json".into()), &ctx);
        assert_eq!(
            app.engine.settings, accepted,
            "an unavailable selection cannot erase usable settings"
        );
        app.apply(Action::SettingsChanged, &ctx);
        assert_eq!(
            ctx.theme(),
            egui::Theme::Light,
            "unrelated settings must keep the custom base"
        );
        for choice in ThemeChoice::ALL {
            app.apply(Action::SetTheme(choice), &ctx);
            assert_eq!(app.engine.settings.theme, choice);
            assert!(app.engine.settings.custom_theme.is_none());
            assert!(app.engine.settings.custom_theme_cache.is_none());
            if choice != ThemeChoice::System {
                assert_eq!(app.palette.dark, choice == ThemeChoice::Dark);
            }
        }
    }

    #[test]
    fn premium_listeners_discover_personal_apps_before_requests_slow_down() {
        let mut app = test_app("personal-app-intro");
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.engine.user = Some(User {
            product: Some("premium".into()),
            ..User::default()
        });
        // Having seen an old transient toast does not count as seeing the intro.
        app.engine.settings.personal_app_nudge_at = Some("2026-09-09T10:00:00Z".into());
        app.engine.maybe_suggest_personal_app();
        assert!(matches!(app.engine.dialog, Some(Dialog::PersonalAppIntro)));
        assert!(!app.engine.settings.personal_app_intro_seen);
        app.engine.actions.push(Action::CloseDialog);
        app.apply_actions(&egui::Context::default());
        assert!(app.engine.settings.personal_app_intro_seen);
        assert!(app.engine.dialog.is_none());
        let saved = serde_json::to_string(&app.engine.settings).unwrap();
        let mut restarted = test_app("personal-app-intro-restart");
        restarted.engine.settings = serde_json::from_str(&saved).unwrap();
        restarted.engine.auth = app.engine.auth.clone();
        restarted.engine.user = app.engine.user.clone();
        restarted.engine.maybe_suggest_personal_app();
        assert!(restarted.engine.dialog.is_none(), "dismissal survives a restart");
        assert!(app.engine.toasts.is_empty(), "the old daily reminder is replaced");
    }

    #[test]
    fn personal_app_intro_waits_for_a_premium_account_using_shared_access() {
        let mut app = test_app("personal-app-intro-eligibility");
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        for product in [None, Some("free"), Some("open")] {
            app.engine.user = Some(User {
                product: product.map(str::to_string),
                ..User::default()
            });
            app.engine.maybe_suggest_personal_app();
            assert!(app.engine.dialog.is_none());
        }
        app.engine.user = Some(User {
            product: Some("premium".into()),
            ..User::default()
        });
        app.engine.settings.web_client_id = Some("personal-client".into());
        app.engine.maybe_suggest_personal_app();
        assert!(app.engine.dialog.is_none());
        app.engine.settings.web_client_id = None;
        app.engine.web_app = Some("personal-client".into());
        app.engine.maybe_suggest_personal_app();
        assert!(app.engine.dialog.is_none());
        app.engine.web_app = None;
        app.engine.auth = AuthStatus::SignedOut;
        app.engine.maybe_suggest_personal_app();
        assert!(app.engine.dialog.is_none());
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.engine.offline = true;
        app.engine.maybe_suggest_personal_app();
        assert!(app.engine.dialog.is_none());
    }

    #[test]
    fn personal_app_intro_defers_while_another_surface_is_in_use() {
        let mut app = test_app("personal-app-intro-defer");
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.engine.user = Some(User {
            product: Some("premium".into()),
            ..User::default()
        });
        app.engine.dialog = Some(Dialog::Shortcuts);
        app.engine.maybe_suggest_personal_app();
        assert!(matches!(app.engine.dialog, Some(Dialog::Shortcuts)));
        app.engine.dialog = None;
        app.engine.show_devices = true;
        app.engine.maybe_suggest_personal_app();
        assert!(app.engine.dialog.is_none());
        app.engine.show_devices = false;
        app.engine.settings.winamp_window = true;
        app.engine.maybe_suggest_personal_app();
        assert!(app.engine.dialog.is_none());
        app.engine.settings.winamp_window = false;
        app.engine.open(Page::Settings);
        app.engine.maybe_suggest_personal_app();
        assert!(app.engine.dialog.is_none());
        app.engine.open(Page::Home);
        app.engine.maybe_suggest_personal_app();
        assert!(matches!(app.engine.dialog, Some(Dialog::PersonalAppIntro)));
        app.engine.handle_auth(AuthStatus::SignedOut);
        assert!(app.engine.dialog.is_none());
        assert!(!app.engine.settings.personal_app_intro_seen);
    }

    fn play(uri: &str, at: &str) -> crate::api::models::PlayHistory {
        crate::api::models::PlayHistory {
            track: crate::api::models::Track {
                uri: uri.to_string(),
                ..Default::default()
            },
            played_at: Some(at.to_string()),
            context: None,
        }
    }

    fn history(
        items: Vec<crate::api::models::PlayHistory>,
        before: Option<&str>,
    ) -> crate::api::models::CursorPage<crate::api::models::PlayHistory> {
        crate::api::models::CursorPage {
            items,
            cursors: Some(crate::api::models::Cursors {
                before: before.map(str::to_string),
                after: None,
            }),
            ..Default::default()
        }
    }

    /// Rule: the Recents tab is a history, so the same song played twice
    /// is two rows. Collapsing repeats would lose what the list is for.
    #[test]
    fn recents_keep_a_song_played_twice() {
        let mut app = test_app("recents-repeat");
        let page = history(
            vec![
                play("spotify:track:a", "2026-09-01T10:00:00Z"),
                play("spotify:track:a", "2026-09-01T09:00:00Z"),
                play("spotify:track:b", "2026-09-01T08:00:00Z"),
            ],
            Some("cursor-1"),
        );
        app.engine.absorb_recents(page, 3);
        assert_eq!(app.engine.recents.items.len(), 3, "both plays of a are kept");
    }

    /// Duplicate play records across page boundaries are removed.
    #[test]
    fn recents_drop_a_play_that_arrives_twice() {
        let mut app = test_app("recents-dedup");
        app.engine.absorb_recents(
            history(
                vec![
                    play("spotify:track:a", "2026-09-01T10:00:00Z"),
                    play("spotify:track:b", "2026-09-01T09:00:00Z"),
                ],
                Some("cursor-1"),
            ),
            2,
        );
        app.engine.absorb_recents(
            history(
                vec![
                    play("spotify:track:b", "2026-09-01T09:00:00Z"),
                    play("spotify:track:c", "2026-09-01T08:00:00Z"),
                ],
                Some("cursor-2"),
            ),
            2,
        );
        let uris: Vec<&str> = app.engine.recents
            .items
            .iter()
            .map(|play| play.track.uri.as_str())
            .collect();
        assert_eq!(
            uris,
            vec!["spotify:track:a", "spotify:track:b", "spotify:track:c"]
        );
    }

    /// A short page ends history pagination, even with a cursor.
    #[test]
    fn a_short_page_ends_the_recents_list() {
        let mut app = test_app("recents-short");
        app.engine.absorb_recents(
            history(
                vec![play("spotify:track:a", "2026-09-01T10:00:00Z")],
                Some("more"),
            ),
            50,
        );
        assert!(
            app.engine.recents.complete,
            "a page of one against fifty is the end"
        );
        assert!(
            app.engine.recents.after.is_none(),
            "and there is nothing to ask for"
        );
    }

    /// A full page with a cursor allows another history request.
    #[test]
    fn a_full_page_leaves_the_recents_list_open() {
        let mut app = test_app("recents-full");
        app.engine.absorb_recents(
            history(
                vec![
                    play("spotify:track:a", "2026-09-01T10:00:00Z"),
                    play("spotify:track:b", "2026-09-01T09:00:00Z"),
                ],
                Some("cursor-1"),
            ),
            2,
        );
        assert!(!app.engine.recents.complete);
        assert_eq!(app.engine.recents.after.as_deref(), Some("cursor-1"));
    }

    /// The sidebar's Recently played order is newest first. Paging back
    /// through Recent reaches plays older than every context already in
    /// that order, so they go after it rather than ahead of it, and a
    /// context played more recently keeps its place.
    #[test]
    fn older_history_pages_do_not_jump_ahead_in_the_sidebar_order() {
        let ctx = egui::Context::default();
        let mut app = test_app("recents-older-contexts");
        let from = |uri: &str, context: &str, at: &str| crate::api::models::PlayHistory {
            context: Some(crate::api::models::Context {
                uri: context.to_string(),
                kind: "playlist".into(),
            }),
            ..play(uri, at)
        };
        let answer = |app: &mut App, page| {
            app.handle_api(ApiResponse::RecentlyPlayed {
                who: RecentsFor::Panel,
                generation: app.engine.recents_generation,
                limit: 2,
                result: Ok(page),
            });
        };
        app.apply(Action::ReloadRecents, &ctx);
        answer(
            &mut app,
            history(
                vec![
                    from(
                        "spotify:track:a",
                        "spotify:playlist:p1",
                        "2026-09-01T10:00:00Z",
                    ),
                    from(
                        "spotify:track:b",
                        "spotify:playlist:p2",
                        "2026-09-01T09:00:00Z",
                    ),
                ],
                Some("cursor-1"),
            ),
        );
        assert_eq!(
            app.engine.recent_contexts,
            ["spotify:playlist:p1", "spotify:playlist:p2"]
        );
        app.apply(Action::LoadMoreRecents, &ctx);
        app.engine.note_recent_context("spotify:album:just-played");
        answer(
            &mut app,
            history(
                vec![
                    from(
                        "spotify:track:c",
                        "spotify:playlist:p3",
                        "2026-09-01T08:00:00Z",
                    ),
                    from(
                        "spotify:track:d",
                        "spotify:artist:x",
                        "2026-09-01T07:30:00Z",
                    ),
                    from(
                        "spotify:track:e",
                        "spotify:playlist:p2",
                        "2026-09-01T07:00:00Z",
                    ),
                ],
                Some("cursor-2"),
            ),
        );
        assert_eq!(app.engine.recents.items.len(), 5, "the older page was taken");
        assert_eq!(
            app.engine.recent_contexts,
            [
                "spotify:album:just-played",
                "spotify:playlist:p1",
                "spotify:playlist:p2",
                "spotify:playlist:p3"
            ],
            "older plays follow the newer ones"
        );

        // A full order has no room left for plays older than all of it.
        app.engine.recent_contexts = (0..RECENT_CONTEXTS_KEPT)
            .map(|index| format!("spotify:playlist:full{index}"))
            .collect();
        let full = app.engine.recent_contexts.clone();
        app.apply(Action::LoadMoreRecents, &ctx);
        answer(
            &mut app,
            history(
                vec![from(
                    "spotify:track:f",
                    "spotify:playlist:p4",
                    "2026-09-01T06:00:00Z",
                )],
                Some("cursor-3"),
            ),
        );
        assert_eq!(app.engine.recents.items.len(), 6, "the oldest page was taken");
        assert_eq!(app.engine.recent_contexts, full);
        app.apply(Action::ReloadRecents, &ctx);
        answer(
            &mut app,
            history(
                vec![from(
                    "spotify:track:g",
                    "spotify:playlist:p4",
                    "2026-09-01T11:00:00Z",
                )],
                None,
            ),
        );
        assert_eq!(app.engine.recent_contexts[0], "spotify:playlist:p4");
        assert_eq!(app.engine.recent_contexts.len(), RECENT_CONTEXTS_KEPT);
    }

    /// Saving to or removing from the library resets that shelf so it is
    /// read again from the top. A page asked for before the reset belongs
    /// to the old list: taking it starts the shelf part way through and
    /// marks its rows saved again, even one that was just removed.
    #[test]
    fn a_page_asked_for_before_a_shelf_reset_is_not_taken() {
        use crate::api::models::{
            Artist, CursorPage, Cursors, Page as ApiPage, SavedAlbum, SavedEpisode, SavedShow,
        };
        fn page<T>(items: Vec<T>, offset: u32) -> ApiPage<T> {
            let end = offset + items.len() as u32;
            ApiPage {
                items,
                total: 3,
                limit: 2,
                offset,
                next: (end < 3).then(|| "more".to_string()),
            }
        }
        let ctx = egui::Context::default();
        let mut app = test_app("library-late-pages");
        let changed = |app: &mut App, uri: &str, saved: bool| {
            app.handle_api(ApiResponse::SavedChanged {
                uris: vec![uri.to_string()],
                saved,
                result: Ok(()),
            });
        };

        // Followed artists continue from a cursor.
        let artists = |names: &[&str], after: Option<&str>| CursorPage {
            items: names
                .iter()
                .map(|name| Artist {
                    uri: format!("spotify:artist:{name}"),
                    ..Artist::default()
                })
                .collect(),
            cursors: Some(Cursors {
                after: after.map(str::to_string),
                before: None,
            }),
            ..CursorPage::default()
        };
        app.apply(Action::LoadMore(Page::Artists), &ctx);
        app.handle_api(ApiResponse::FollowedArtists {
            after: None,
            result: Ok(artists(&["a", "b"], Some("page-2"))),
        });
        app.apply(Action::LoadMore(Page::Artists), &ctx);
        changed(&mut app, "spotify:artist:new", true);
        app.apply(Action::LoadMore(Page::Artists), &ctx);
        app.handle_api(ApiResponse::FollowedArtists {
            after: Some("page-2".into()),
            result: Ok(artists(&["c"], None)),
        });
        assert!(
            app.engine.library.artists.items.is_empty() && app.engine.library.artists.loading,
            "the reset shelf waits for its own first page"
        );
        app.handle_api(ApiResponse::FollowedArtists {
            after: None,
            result: Ok(artists(&["new", "a"], Some("page-2"))),
        });
        let followed: Vec<&str> = app.engine.library
            .artists
            .items
            .iter()
            .map(|artist| artist.uri.as_str())
            .collect();
        assert_eq!(followed, ["spotify:artist:new", "spotify:artist:a"]);
        assert_eq!(app.engine.library.artists.after.as_deref(), Some("page-2"));

        // Albums, podcasts and episodes continue from an offset.
        let album = |id: &str| SavedAlbum {
            album: web_album(&format!("spotify:album:{id}"), "album", Some("album")),
            ..SavedAlbum::default()
        };
        app.apply(Action::LoadMore(Page::Albums), &ctx);
        app.handle_api(ApiResponse::SavedAlbums {
            offset: 0,
            result: Ok(page(vec![album("a"), album("b")], 0)),
        });
        app.apply(Action::LoadMore(Page::Albums), &ctx);
        changed(&mut app, "spotify:album:c", false);
        app.handle_api(ApiResponse::SavedAlbums {
            offset: 2,
            result: Ok(page(vec![album("c")], 2)),
        });
        assert!(app.engine.library.albums.items.is_empty());
        assert!(
            !app.engine.library.albums.loaded_once,
            "the shelf still starts from the top"
        );
        assert_eq!(
            app.engine.is_saved("spotify:album:c"),
            Some(false),
            "a removed album stays removed"
        );

        let show = |id: &str| SavedShow {
            show: crate::api::models::Show {
                uri: format!("spotify:show:{id}"),
                ..Default::default()
            },
            ..SavedShow::default()
        };
        app.apply(Action::LoadMore(Page::Podcasts), &ctx);
        app.handle_api(ApiResponse::SavedShows {
            offset: 0,
            result: Ok(page(vec![show("a"), show("b")], 0)),
        });
        app.apply(Action::LoadMore(Page::Podcasts), &ctx);
        changed(&mut app, "spotify:show:new", true);
        app.handle_api(ApiResponse::SavedShows {
            offset: 2,
            result: Ok(page(vec![show("c")], 2)),
        });
        assert!(app.engine.library.shows.items.is_empty() && !app.engine.library.shows.loaded_once);

        let episode = |id: &str| SavedEpisode {
            episode: crate::api::models::Episode {
                uri: format!("spotify:episode:{id}"),
                ..Default::default()
            },
            ..SavedEpisode::default()
        };
        app.apply(Action::LoadMore(Page::Episodes), &ctx);
        app.handle_api(ApiResponse::SavedEpisodes {
            offset: 0,
            result: Ok(page(vec![episode("a"), episode("b")], 0)),
        });
        app.apply(Action::LoadMore(Page::Episodes), &ctx);
        changed(&mut app, "spotify:episode:new", true);
        app.handle_api(ApiResponse::SavedEpisodes {
            offset: 2,
            result: Ok(page(vec![episode("c")], 2)),
        });
        assert!(app.engine.library.episodes.items.is_empty() && !app.engine.library.episodes.loaded_once);
        app.engine.backend.shutdown();
        let _ = std::fs::remove_dir_all(app.engine.dirs.config.parent().unwrap());
    }

    /// Home's podcast shelf reads a bounded number of saved shows once per
    /// Home refresh, skips known audiobooks, and keeps what it shows until
    /// the current refresh answers.
    #[test]
    fn home_reads_a_few_saved_podcasts_once_per_refresh() {
        use crate::api::models::{Episode, Page as ApiPage, SavedShow};
        let mut app = test_app("home-podcasts");
        let show = |index: usize| Show {
            id: format!("s{index}"),
            uri: format!("spotify:show:s{index}"),
            ..Show::default()
        };
        app.engine.load_home(false);
        assert!(app.engine.library.shows.loading, "Home asks for the saved shows");
        assert!(app.engine.backend.take_home_episode_requests().is_empty());

        app.engine.audiobook_shows.insert("spotify:show:s1".into());
        app.handle_api(ApiResponse::SavedShows {
            offset: 0,
            result: Ok(ApiPage {
                items: (0..12)
                    .map(|index| SavedShow {
                        show: show(index),
                        ..SavedShow::default()
                    })
                    .collect(),
                total: 12,
                limit: 50,
                offset: 0,
                next: None,
            }),
        });
        let generation = app.engine.home.generation;
        let expected: Vec<String> = [0, 2, 3, 4, 5, 6, 7, 8]
            .iter()
            .map(|index| format!("s{index}"))
            .collect();
        assert_eq!(
            app.engine.backend.take_home_episode_requests(),
            vec![(expected, generation)]
        );

        // Visiting Home again soon, or another first page of the shows,
        // asks for nothing more.
        app.engine.load_home(false);
        app.engine.library.shows.next_offset = Some(0);
        app.handle_api(ApiResponse::SavedShows {
            offset: 0,
            result: Ok(ApiPage {
                items: vec![SavedShow {
                    show: show(0),
                    ..SavedShow::default()
                }],
                total: 1,
                limit: 50,
                offset: 0,
                next: None,
            }),
        });
        assert!(app.engine.backend.take_home_episode_requests().is_empty());

        let episodes = |show_index: usize| {
            vec![(
                show(show_index),
                vec![Episode {
                    uri: format!("spotify:episode:e{show_index}"),
                    ..Episode::default()
                }],
            )]
        };
        app.handle_api(ApiResponse::HomeEpisodes {
            generation,
            result: Ok(episodes(0)),
        });
        assert_eq!(app.engine.home.podcasts, episodes(0));

        app.engine.load_home(true);
        assert_eq!(app.engine.backend.take_home_episode_requests().len(), 1);
        app.handle_api(ApiResponse::HomeEpisodes {
            generation,
            result: Ok(episodes(2)),
        });
        assert_eq!(app.engine.home.podcasts, episodes(0), "an older answer is ignored");
        app.handle_api(ApiResponse::HomeEpisodes {
            generation: app.engine.home.generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(app.engine.home.podcasts, episodes(0), "a failure keeps the shelf");
        app.engine.backend.shutdown();
        let _ = std::fs::remove_dir_all(app.engine.dirs.config.parent().unwrap());
    }

    /// Closing and reopening restores queue rows and their manual split.
    #[test]
    fn the_queue_comes_back_after_a_restart() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-queue-restart-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let options = AppOptions {
            media_controls: false,
            restore_sign_in: false,
            tray: false,
        };
        let mut app = App::new(
            &Waker::default(),
            dirs.clone(),
            Settings::default(),
            options,
        );
        app.engine.local_ready = true;
        app.engine.resume_track = Some("spotify:track:a".into());
        app.engine.manual_queue = vec!["spotify:track:b".into()];
        app.engine.queue = loaded_queue(
            "spotify:track:a",
            &["spotify:track:b", "spotify:track:ctx1"],
        );
        app.save_session();
        app.engine.flush_files();

        let options = AppOptions {
            media_controls: false,
            restore_sign_in: false,
            tray: false,
        };
        let app = App::new(&Waker::default(), dirs, Settings::default(), options);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b", "spotify:track:ctx1"],
            "the queue is shown as it was left"
        );
        assert_eq!(
            app.engine.queued_rows_len(),
            1,
            "the remembered hand-queued song keeps its own section"
        );
    }

    /// Saving the queue writes the playing song and every row after it,
    /// each song once, in playing order.
    #[test]
    fn saving_the_queue_writes_each_song_once_in_order() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.engine.queue = loaded_queue(
            "spotify:track:a",
            &[
                "spotify:track:b",
                "spotify:track:a",
                "spotify:track:b",
                "spotify:track:c",
            ],
        );
        assert_eq!(
            app.engine.queue_playlist_uris(),
            vec!["spotify:track:a", "spotify:track:b", "spotify:track:c"],
            "the playing song leads and a repeat wrap adds nothing"
        );
    }

    /// A saved song radio is named after its song; any other queue is
    /// named after the day.
    #[test]
    fn a_saved_radio_is_named_after_its_song() {
        let mut app = headless_app();
        app.engine.track_cache.insert(
            "xyz".into(),
            crate::api::models::Track {
                id: Some("xyz".into()),
                uri: "spotify:track:xyz".into(),
                name: "Wish You Were Here".into(),
                ..Default::default()
            },
        );
        app.engine.assumed_context = Some(AssumedContext {
            uri: "spotify:station:track:xyz".into(),
            shuffle: None,
            at: Instant::now(),
        });
        assert_eq!(app.engine.queue_playlist_name(), "Wish You Were Here Radio");
        app.engine.assumed_context = None;
        assert!(app.engine.queue_playlist_name().starts_with("Queue "));
    }

    #[test]
    fn translated_context_labels_preserve_spotify_names() {
        use crate::i18n::LOCALES;
        let mut app = headless_app();
        let today = jiff::Zoned::now().strftime("%Y-%m-%d").to_string();
        let title = "Home {date} 夜";
        app.engine.track_cache.insert(
            "xyz".into(),
            crate::api::models::Track {
                id: Some("xyz".into()),
                uri: "spotify:track:xyz".into(),
                name: title.into(),
                ..Default::default()
            },
        );
        app.engine.library.playlists = Loadable::Loaded(vec![crate::api::models::Playlist {
            id: "pl9".into(),
            uri: "spotify:playlist:pl9".into(),
            name: title.into(),
            ..Default::default()
        }]);
        for &locale in LOCALES {
            app.engine.locale = locale;
            app.engine.assumed_context = None;
            assert_eq!(
                app.engine.queue_playlist_name(),
                gettext(locale, "Queue {date}").replace("{date}", &today)
            );
            for (uri, expected, page) in [
                (
                    "spotify:playlist:pl9",
                    title.into(),
                    Some(Page::Playlist("pl9".into())),
                ),
                (
                    "spotify:playlist:unloaded",
                    gettext(locale, "Playlist").into_owned(),
                    Some(Page::Playlist("unloaded".into())),
                ),
                (
                    "spotify:user:me:collection",
                    gettext(locale, "Liked Songs").into_owned(),
                    Some(Page::LikedSongs),
                ),
                (
                    "spotify:station:track:xyz",
                    gettext(locale, "{track} Radio").replace("{track}", title),
                    Some(Page::Radio("spotify:track:xyz".into())),
                ),
            ] {
                app.engine.assumed_context = Some(AssumedContext {
                    uri: uri.into(),
                    shuffle: None,
                    at: Instant::now(),
                });
                let from = app.engine.playing_from().unwrap();
                assert_eq!(from.name, expected);
                assert_eq!(from.page, page);
                if uri.starts_with("spotify:station:") {
                    assert_eq!(app.engine.queue_playlist_name(), expected);
                }
            }
        }
    }

    /// The queue names where the playing song comes from and opens it.
    /// A radio has no page; a context whose name has not loaded is still
    /// named by kind so it can open.
    #[test]
    fn playing_from_names_the_context_and_its_page() {
        let mut app = headless_app();
        assert_eq!(app.engine.playing_from(), None, "no context, no line");
        let assume = |app: &mut App, uri: &str| {
            app.engine.assumed_context = Some(AssumedContext {
                uri: uri.into(),
                shuffle: None,
                at: Instant::now(),
            });
        };
        let named = |app: &App| {
            let from = app.engine.playing_from().expect("a context is playing");
            (from.name, from.page)
        };

        app.engine.library.playlists = Loadable::Loaded(vec![crate::api::models::Playlist {
            id: "pl9".into(),
            uri: "spotify:playlist:pl9".into(),
            name: "Long Way Home".into(),
            ..Default::default()
        }]);
        assume(&mut app, "spotify:playlist:pl9");
        assert_eq!(
            named(&app),
            ("Long Way Home".into(), Some(Page::Playlist("pl9".into())))
        );
        assume(&mut app, "spotify:playlist:unloaded");
        assert_eq!(
            named(&app),
            ("Playlist".into(), Some(Page::Playlist("unloaded".into())))
        );

        app.engine.album_pages.insert(
            "alb1".into(),
            AlbumPage {
                album: Loadable::Loaded(crate::api::models::Album {
                    id: "alb1".into(),
                    uri: "spotify:album:alb1".into(),
                    name: "Black Sands".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        assume(&mut app, "spotify:album:alb1");
        assert_eq!(
            named(&app),
            ("Black Sands".into(), Some(Page::Album("alb1".into())))
        );

        assume(&mut app, "spotify:user:me:collection");
        assert_eq!(named(&app), ("Liked Songs".into(), Some(Page::LikedSongs)));

        app.engine.track_cache.insert(
            "xyz".into(),
            crate::api::models::Track {
                id: Some("xyz".into()),
                uri: "spotify:track:xyz".into(),
                name: "Wish You Were Here".into(),
                ..Default::default()
            },
        );
        assume(&mut app, "spotify:station:track:xyz");
        assert_eq!(
            named(&app),
            (
                "Wish You Were Here Radio".into(),
                Some(Page::Radio("spotify:track:xyz".into()))
            )
        );
        assume(&mut app, "spotify:station:track:uncached");
        assert_eq!(
            named(&app),
            (
                "Radio".into(),
                Some(Page::Radio("spotify:track:uncached".into()))
            )
        );
        assume(&mut app, "spotify:station:playlist:pl9");
        assert_eq!(
            named(&app),
            (
                "Long Way Home Radio".into(),
                Some(Page::Radio("spotify:playlist:pl9".into()))
            )
        );
    }

    /// A replaced window, as when the mini player's taskbar setting
    /// changes, is titled with the playing song again, not left as
    /// "Spotifast".
    #[test]
    fn a_new_window_is_titled_with_the_playing_song() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.attach(&ctx);
        app.window_title = "Bonobo - Rosewood".into();
        app.attach(&ctx);
        assert!(
            app.window_title.is_empty(),
            "the next frame sends the title again"
        );
    }

    fn radio_song(id: &str, artist: &str) -> Track {
        Track {
            id: Some(id.into()),
            uri: format!("spotify:track:{id}"),
            name: format!("Song {id}"),
            duration_ms: 200_000,
            artists: vec![crate::api::models::ArtistRef {
                name: artist.into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn draw_radio(
        ctx: &egui::Context,
        app: &mut App,
        seed: &str,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1240.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| crate::ui::radio::radio(app, ui, seed),
        );
        output.textures_delta.clear();
        app.apply_actions(ctx);
        output.platform_output.accesskit_update.unwrap()
    }

    fn click_labelled(
        ctx: &egui::Context,
        app: &mut App,
        seed: &str,
        label: &str,
    ) -> egui::accesskit::TreeUpdate {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, TreeId};
        let tree = draw_radio(ctx, app, seed, Vec::new());
        let id = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .unwrap_or_else(|| panic!("a {label} control"))
            .0;
        draw_radio(
            ctx,
            app,
            seed,
            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                target_tree: TreeId::ROOT,
                target_node: id,
                action: AccessibleAction::Click,
                data: None,
            })],
        )
    }

    /// #369: Go to song radio opens the radio's page, as in Spotify's app,
    /// and plays nothing until asked.
    #[test]
    fn song_radio_opens_its_page_without_playing() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "test".into(),
        };
        app.apply(Action::Open(Page::Radio("spotify:track:xyz".into())), &ctx);
        assert_eq!(app.engine.page(), &Page::Radio("spotify:track:xyz".into()));
        assert!(matches!(
            app.engine.radio_pages["spotify:track:xyz"].songs,
            Loadable::Loading
        ));
        assert!(app.engine.queued_play.is_none());
        assert!(app.engine.optimistic_playing.is_none());
        assert!(!app.engine.show_queue_panel);
        assert_eq!(
            Page::decode(&Page::Radio("spotify:playlist:pl9".into()).encode()),
            Some(Page::Radio("spotify:playlist:pl9".into()))
        );
        assert_eq!(Page::decode("radio:spotify:show:abc"), None);
    }

    /// Spotify mixes a station afresh each time it is asked, so the page's
    /// Play plays the songs on screen, shuffled or not, as the radio.
    #[test]
    fn a_radio_plays_the_songs_it_shows() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::theme::install(&ctx);
        let seed = "spotify:playlist:pl9";
        for shuffle in [false, true] {
            let mut app = headless_app();
            app.engine.auth = AuthStatus::Connected {
                username: "test".into(),
            };
            app.engine.backend.set_offline(true);
            app.engine.shuffle_wanted = shuffle;
            app.apply(Action::Open(Page::Radio(seed.into())), &ctx);
            let generation = app.engine.radio_pages[seed].generation;
            let songs = vec![radio_song("a", "Björk"), radio_song("b", "Arca")];
            app.engine.receive_radio(seed, generation, Ok(songs));
            click_labelled(&ctx, &mut app, seed, "Play");
            assert_eq!(
                app.engine.queued_play.as_ref().expect("a play request").uris,
                vec!["spotify:track:a".to_string(), "spotify:track:b".into()],
                "shuffle {shuffle}: the shown songs play"
            );
            assert_eq!(
                app.engine.playing_context_uri().as_deref(),
                Some("spotify:station:playlist:pl9"),
                "the queue names the radio"
            );
            app.engine.backend.shutdown();
        }
    }

    /// An answer for an earlier request does not replace the page, a
    /// failure can be retried, and Refresh asks for a new mix.
    #[test]
    fn a_radio_takes_only_its_latest_answer() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "test".into(),
        };
        let seed = "spotify:album:alb1";
        app.apply(Action::Open(Page::Radio(seed.into())), &ctx);
        let first = app.engine.radio_pages[seed].generation;
        app.engine.receive_radio(
            seed,
            first,
            Err("Couldn't load this radio. Try again.".into()),
        );
        assert!(matches!(app.engine.radio_pages[seed].songs, Loadable::Failed(_)));
        app.apply(Action::Reload(Page::Radio(seed.into())), &ctx);
        let second = app.engine.radio_pages[seed].generation;
        assert_ne!(first, second);
        app.engine.receive_radio(seed, first, Ok(vec![radio_song("old", "Old")]));
        assert!(matches!(app.engine.radio_pages[seed].songs, Loadable::Loading));
        app.engine.receive_radio(seed, second, Ok(vec![radio_song("new", "New")]));
        let songs = app.engine.radio_pages[seed].songs.get().expect("the latest mix");
        assert_eq!(songs[0].uri, "spotify:track:new");
        assert!(app.engine.track_cache.contains_key("new"));
    }

    /// Refresh keeps the mix on screen until the new one arrives, shows the
    /// new songs once it does, and keeps the old ones if it fails.
    #[test]
    fn refreshing_a_radio_keeps_its_songs_until_the_new_mix_arrives() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::theme::install(&ctx);
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "test".into(),
        };
        let seed = "spotify:artist:art1";
        app.apply(Action::Open(Page::Radio(seed.into())), &ctx);
        let generation = app.engine.radio_pages[seed].generation;
        app.engine.receive_radio(seed, generation, Ok(vec![radio_song("old", "Old")]));
        let rows = |app: &mut App| {
            draw_radio(&ctx, app, seed, Vec::new());
            app.engine.table_rows[&Page::Radio(seed.into())].items[0]
                .0
                .uri()
                .to_string()
        };
        assert_eq!(rows(&mut app), "spotify:track:old");

        app.apply(Action::Reload(Page::Radio(seed.into())), &ctx);
        assert!(app.engine.radio_pages[seed].refreshing);
        assert_eq!(rows(&mut app), "spotify:track:old", "the old mix stays");
        let asked = app.engine.radio_pages[seed].generation;
        app.engine.receive_radio(seed, asked, Ok(vec![radio_song("new", "New")]));
        assert!(!app.engine.radio_pages[seed].refreshing);
        assert_eq!(
            rows(&mut app),
            "spotify:track:new",
            "the table shows the new mix"
        );

        app.apply(Action::Reload(Page::Radio(seed.into())), &ctx);
        let asked = app.engine.radio_pages[seed].generation;
        app.engine.receive_radio(
            seed,
            asked,
            Err("Couldn't load this radio. Try again.".into()),
        );
        assert_eq!(
            rows(&mut app),
            "spotify:track:new",
            "a failed refresh keeps the songs"
        );
        app.engine.backend.shutdown();
    }

    /// Save as playlist keeps the mix on screen, in order, under the
    /// radio's name.
    #[test]
    fn saving_a_radio_makes_a_playlist_of_its_songs() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::theme::install(&ctx);
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "test".into(),
        };
        app.engine.backend.set_offline(true);
        let seed = "spotify:track:xyz";
        app.engine.track_cache.insert("xyz".into(), {
            let mut seed_song = radio_song("xyz", "Pink Floyd");
            seed_song.name = "Wish You Were Here".into();
            seed_song
        });
        app.apply(Action::Open(Page::Radio(seed.into())), &ctx);
        let generation = app.engine.radio_pages[seed].generation;
        app.engine.receive_radio(
            seed,
            generation,
            Ok(vec![radio_song("b", "Camel"), radio_song("a", "Yes")]),
        );
        let tree = draw_radio(&ctx, &mut app, seed, Vec::new());
        assert_eq!(
            app.engine.radio_name(seed).as_deref(),
            Some("Wish You Were Here Radio"),
            "the page is named after its song"
        );
        let id = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Save as playlist"))
            .expect("a Save as playlist button")
            .0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1240.0, 800.0),
                )),
                events: vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        target_tree: egui::accesskit::TreeId::ROOT,
                        target_node: id,
                        action: egui::accesskit::Action::Click,
                        data: None,
                    },
                )],
                ..Default::default()
            },
            |ui| crate::ui::radio::radio(&mut app, ui, seed),
        );
        output.textures_delta.clear();
        let saved = std::mem::take(&mut app.engine.actions);
        assert!(matches!(saved.as_slice(), [Action::SaveRadio(uri)] if uri == seed));
        app.apply(Action::SaveRadio(seed.into()), &ctx);
        assert!(
            app.engine.actions.iter().any(|action| matches!(
                action,
                Action::CreatePlaylist { name, public: false, add_uris }
                    if name == "Wish You Were Here Radio"
                        && add_uris == &["spotify:track:b".to_string(), "spotify:track:a".into()]
            )),
            "{:?}",
            app.engine.actions
        );
        app.engine.backend.shutdown();
    }

    /// MilkDrop playback keys produce the same actions as the main window.
    #[cfg(feature = "milkdrop")]
    #[test]
    fn the_milkdrop_window_drives_playback() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;

        for command in [
            "play-pause",
            "next",
            "previous",
            "mute",
            "save-toggle",
            "shuffle",
            "volume-up",
            "volume-down",
        ] {
            app.engine.actions.clear();
            app.milkdrop_command(command);
            assert_eq!(
                app.engine.actions.len(),
                1,
                "{command} asks the player for one thing"
            );
        }

        app.engine.actions.clear();
        app.milkdrop_command("save-toggle");
        assert!(matches!(
            app.engine.actions.first(),
            Some(Action::ToggleSaved(uri)) if uri == "spotify:track:a"
        ));
        app.engine.actions.clear();
        app.milkdrop_command("next");
        assert!(matches!(app.engine.actions.first(), Some(Action::Next)));
        app.engine.actions.clear();
        app.milkdrop_command("volume-down");
        assert!(matches!(app.engine.actions.first(), Some(Action::VolumeBy(-5))));

        // Ignore unknown commands.
        app.engine.actions.clear();
        app.milkdrop_command("teleport");
        assert!(app.engine.actions.is_empty());
    }

    /// The first reported screen rate sets the default FPS, but later reports
    /// do not override a configured value.
    #[cfg(feature = "milkdrop")]
    #[test]
    fn the_frame_rate_matches_the_screen_the_first_time_it_is_known() {
        let mut app = headless_app();
        assert_eq!(app.engine.settings.milkdrop_screen_hz, 0, "no screen has spoken");
        assert_eq!(app.engine.settings.milkdrop_fps, crate::milkdrop::DEFAULT_FPS);

        app.learn_screen_hz(1);
        assert_eq!(
            app.engine.settings.milkdrop_screen_hz, 0,
            "Windows's hardware default is no rate"
        );
        assert_eq!(app.engine.settings.milkdrop_fps, crate::milkdrop::DEFAULT_FPS);

        app.learn_screen_hz(144);
        assert_eq!(app.engine.settings.milkdrop_screen_hz, 144);
        assert_eq!(app.engine.settings.milkdrop_fps, 144, "smooth without being asked");

        // Keep the configured FPS when the screen changes.
        app.engine.settings.milkdrop_fps = 30;
        app.learn_screen_hz(60);
        assert_eq!(
            app.engine.settings.milkdrop_screen_hz, 60,
            "the new screen is noted"
        );
        assert_eq!(app.engine.settings.milkdrop_fps, 30, "their number stands");
    }

    /// The visualizers' dial offers the rate of the screen showing the main
    /// window, MilkDrop's until that screen has said, and none before
    /// either has.
    #[test]
    fn the_visualizer_dial_offers_the_main_windows_screen() {
        let mut app = headless_app();
        assert_eq!(app.visualiser_screen_hz(), 0, "no screen has said");
        app.engine.settings.milkdrop_screen_hz = 144;
        assert_eq!(app.visualiser_screen_hz(), 144, "MilkDrop's meanwhile");
        app.screen_hz = 165;
        assert_eq!(app.visualiser_screen_hz(), 165, "the window's own");
        app.screen_hz = 0;
        assert_eq!(app.visualiser_screen_hz(), 144, "an unusable answer");
    }

    /// A window in the Dock is drawn no frames, so the one frame a Show
    /// request buys has to be the frame that brings it back. Focus alone
    /// leaves it down there, and nothing asks again.
    #[test]
    fn showing_a_minimized_window_restores_it_before_focusing_it() {
        // #given a window exists, minimized rather than closed
        let mut app = headless_app();
        let ctx = egui::Context::default();
        assert!(!app.window_hidden, "a window this app still owns");

        // #when something asks for the window: the Dock, the tray, or
        // `spotifast show`
        let mut output = ctx.run_ui(Default::default(), |ui| {
            app.apply(Action::ShowWindow, ui.ctx());
        });
        output.textures_delta.clear();

        // #then
        let commands = &output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("the root viewport")
            .commands;
        assert!(
            commands.contains(&egui::ViewportCommand::Minimized(false)),
            "asked to show, but never asked to come out of the Dock: {commands:?}"
        );
        assert!(
            commands.contains(&egui::ViewportCommand::Focus),
            "restored but left behind whatever is in front of it: {commands:?}"
        );
    }

    #[test]
    fn repeated_show_requests_never_close_or_hide_the_window() {
        for hidden in [false, true] {
            let mut app = headless_app();
            app.window_hidden = hidden;
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(Default::default(), |ui| {
                app.apply(Action::ShowWindow, ui.ctx());
                app.apply(Action::ShowWindow, ui.ctx());
            });
            output.textures_delta.clear();
            assert!(!app.hide_intent);
            assert_eq!(app.wants_show, hidden);
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            assert!(!commands.contains(&egui::ViewportCommand::Close));
            if !hidden {
                assert!(commands.contains(&egui::ViewportCommand::Focus));
            }
            app.engine.backend.shutdown();
        }
    }

    /// The media controls are handed a downloaded file, never a URL: macOS
    /// loads cover art itself and dereferences a failed load without
    /// checking it, so a URL that does not answer aborts the process. The
    /// sync runs every frame, so the answer is remembered -- which means
    /// emptying the cache has to forget it, or the path outlives the file.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn the_media_controls_only_hear_about_artwork_that_exists() {
        // #given
        let mut app = headless_app();
        let ctx = egui::Context::default();
        let url = "https://i.scdn.co/image/abc";
        let file = app.engine.dirs.cache.join("art").join("0badc0de");
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("the art cache");
        std::fs::write(&file, b"jpeg-ish").expect("a cached file");

        // #then nothing has been downloaded for this song yet
        assert_eq!(app.media_art_file(&ctx, url), None);
        assert!(
            !app.art.prefetch(&ctx, url),
            "the miss should have started the download"
        );

        // #when the cache holds it, that file is what the controls are told
        app.media_art = Some((url.to_owned(), file.clone()));
        assert_eq!(app.media_art_file(&ctx, url), Some(file.clone()));

        // #then another song is not covered by what is remembered
        assert_eq!(
            app.media_art_file(&ctx, "https://i.scdn.co/image/def"),
            None
        );

        // #when the artwork cache is emptied, the remembered path goes too
        app.engine.actions.push(Action::ClearArtCache);
        app.apply_actions(&ctx);
        assert_eq!(app.media_art, None, "a path into a deleted cache");

        let _ = std::fs::remove_file(&file);
    }

    /// Changes the conditions as the power reader would.
    fn change_power(app: &App, change: impl FnOnce(&mut crate::power::Conditions)) {
        let mut conditions = app.power.conditions();
        change(&mut conditions);
        app.power.set_conditions(conditions);
    }

    /// On a metered connection the media controls go without the large
    /// cover rather than download one for every song.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_metered_connection_downloads_no_cover_for_the_controls() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        let url = "https://i.scdn.co/image/metered";
        change_power(&app, |conditions| conditions.metered = true);
        assert_eq!(app.media_art_file(&ctx, url), None);
        assert!(
            app.art.prefetch(&ctx, url),
            "nothing should have asked for it before"
        );
    }

    /// The daily update check waits for a connection that is not metered,
    /// and, where the app follows the system's word on it, for the internet.
    /// Where it does not (Windows), a "no internet" hint holds nothing back.
    #[test]
    fn a_metered_connection_puts_off_the_update_check() {
        for (metered, online, follows, checked, why) in [
            (false, true, true, true, "not checked on a free network"),
            (true, true, false, false, "checked on a metered network"),
            (false, false, true, false, "checked without internet"),
            (
                false,
                false,
                false,
                true,
                "held by a hint it does not follow",
            ),
        ] {
            let mut app = headless_app();
            app.engine.settings.check_for_updates = true;
            app.last_update_check = None;
            app.follows_reachability = follows;
            change_power(&app, |conditions| {
                conditions.metered = metered;
                conditions.online = online;
            });
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.background_frame(ui.ctx());
            });
            output.textures_delta.clear();
            assert_eq!(app.last_update_check.is_some(), checked, "{why}");
        }
    }

    /// An automatic download put off on a metered connection starts once the
    /// connection is not, rather than at the next daily check, and not while
    /// the connection is down: a download that fails would stop the next
    /// automatic one.
    #[test]
    fn an_update_held_on_a_metered_connection_downloads_when_it_is_not() {
        use crate::updates::{DownloadState, Installation, Kind};
        let mut app = headless_app();
        app.engine.settings.download_updates_automatically = true;
        app.engine.settings.check_for_updates = false;
        // Where the app follows the system's word on reaching the internet.
        app.follows_reachability = true;
        app.update = Some(crate::updates::Release {
            version: "9.9.9".into(),
            url: "https://example.invalid/release".into(),
        });
        change_power(&app, |conditions| conditions.metered = true);
        app.handle_backend_events(vec![Event::UpdateSupport(Ok(Installation {
            executable: "/test/spotifast".into(),
            kind: Kind::Portable,
        }))]);
        let ctx = egui::Context::default();
        let pass = |app: &mut App| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.background_frame(ui.ctx());
            });
            output.textures_delta.clear();
        };
        pass(&mut app);
        assert!(
            matches!(app.update_download, DownloadState::Idle),
            "downloaded on a metered network"
        );

        // A metered connection that drops reads as neither metered nor
        // online: still nothing to download over.
        change_power(&app, |conditions| {
            conditions.metered = false;
            conditions.online = false;
        });
        pass(&mut app);
        assert!(
            matches!(app.update_download, DownloadState::Idle),
            "downloaded without internet"
        );

        change_power(&app, |conditions| conditions.online = true);
        pass(&mut app);
        assert!(matches!(
            app.update_download,
            DownloadState::Downloading { .. }
        ));
    }

    /// A step held while automatic downloads are off waits for them to be
    /// turned back on, rather than being dropped when the network comes back.
    #[test]
    fn a_held_update_waits_for_automatic_downloads_to_be_turned_on() {
        use crate::updates::{DownloadState, Installation, Kind};
        let mut app = headless_app();
        app.engine.settings.download_updates_automatically = true;
        app.engine.settings.check_for_updates = false;
        app.update = Some(crate::updates::Release {
            version: "9.9.9".into(),
            url: "https://example.invalid/release".into(),
        });
        change_power(&app, |conditions| conditions.metered = true);
        app.handle_backend_events(vec![Event::UpdateSupport(Ok(Installation {
            executable: "/test/spotifast".into(),
            kind: Kind::Portable,
        }))]);
        let ctx = egui::Context::default();
        let pass = |app: &mut App| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.background_frame(ui.ctx());
            });
            output.textures_delta.clear();
        };
        pass(&mut app);
        assert!(app.held_update.is_some());

        app.engine.settings.download_updates_automatically = false;
        change_power(&app, |conditions| conditions.metered = false);
        pass(&mut app);
        assert!(matches!(app.update_download, DownloadState::Idle));
        assert!(app.held_update.is_some(), "dropped while turned off");

        app.engine.settings.download_updates_automatically = true;
        pass(&mut app);
        assert!(matches!(
            app.update_download,
            DownloadState::Downloading { .. }
        ));
    }

    /// MPRIS hands the desktop the artwork URL and reads no file, so the
    /// controls have nothing to download for it: the full-size cover is
    /// fetched on the platforms that load the image themselves.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_media_controls_are_told_no_file_and_download_nothing() {
        // #given
        let mut app = headless_app();
        let ctx = egui::Context::default();
        let url = "https://i.scdn.co/image/abc";

        // #then MPRIS is left to the URL alone
        assert_eq!(app.media_art_file(&ctx, url), None);
        assert!(
            app.art.prefetch(&ctx, url),
            "a download was started for artwork nothing here reads"
        );
    }

    /// Emptying the artwork cache has to let the cover come back. The files
    /// are gone, so what the loader remembers of them goes too: an entry
    /// left behind answers the next request with a file that is no longer
    /// there, and the playing song stays coverless until the track changes.
    #[test]
    fn clearing_the_artwork_cache_lets_the_cover_come_back() {
        // #given artwork that has already been asked for
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.attach(&ctx);
        let url = "https://i.scdn.co/image/abc";
        assert!(app.art.prefetch(&ctx, url), "the first request");

        // #when the artwork cache is emptied
        app.engine.actions.push(Action::ClearArtCache);
        app.apply_actions(&ctx);

        // #then the next request downloads it again
        assert!(
            app.art.prefetch(&ctx, url),
            "the loader still remembers artwork that has been deleted"
        );
    }

    #[test]
    fn thumbnail_transport_tracks_optimistic_pause_and_window_recreation() {
        use crate::thumbbar::{Icon, ThumbCommand, buttons};
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let ctx = egui::Context::default();
        assert!(!app.thumb_state(true).has_track);
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:thumbnail".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        let state = app.thumb_state(true);
        assert!(state.has_track && state.playing && state.can_control);
        assert_eq!(buttons(&state)[1].icon, Icon::Pause);
        app.apply(ThumbCommand::PlayPause.action(&state).unwrap(), &ctx);
        assert_eq!(
            buttons(&app.thumb_state(true))[1].icon,
            Icon::Play,
            "pause updates before a backend reply"
        );
        app.window_gone();
        app.attach(&ctx);
        assert_eq!(buttons(&app.thumb_state(false))[1].icon, Icon::Play);
        assert!(!app.thumb_state(false).dark);
        app.engine.backend.shutdown();
    }

    #[test]
    fn changing_mini_taskbar_visibility_recreates_only_an_open_mini_window() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let ctx = egui::Context::default();
        app.apply(Action::SetWinampTaskbar(false), &ctx);
        assert!(!app.engine.settings.winamp_show_taskbar);
        assert!(!app.switch_intent, "settings do not close the main window");
        app.engine.settings.winamp_window = true;
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:continues".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        let mut output = ctx.run_ui(egui::RawInput::default(), |_ui| {
            app.apply(Action::SetWinampTaskbar(true), &ctx);
        });
        output.textures_delta.clear();
        assert!(app.engine.settings.winamp_window && app.switch_intent);
        assert!(!app.hide_intent && !app.engine.quit_requested);
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );
        assert_eq!(app.engine.local.playback, Playback::Playing);
        assert_eq!(
            app.engine.local.track.as_ref().unwrap().uri,
            "spotify:track:continues"
        );
        app.switch_intent = false;
        let mut output = ctx.run_ui(egui::RawInput::default(), |_ui| {
            app.apply(Action::SetWinampTaskbar(true), &ctx);
        });
        output.textures_delta.clear();
        assert!(!app.switch_intent);
        assert!(
            !output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );
        app.apply(Action::ToggleWinampWindow, &ctx);
        assert!(
            !app.engine.settings.winamp_window,
            "returning to the main interface remains available"
        );
        app.engine.backend.shutdown();
    }

    #[test]
    fn lyrics_fullscreen_restores_the_previous_window_mode() {
        for was_fullscreen in [false, true] {
            let mut app = headless_app();
            let ctx = egui::Context::default();
            let mut input = egui::RawInput::default();
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .fullscreen = Some(was_fullscreen);
            let mut output = ctx.run_ui(input, |ui| {
                app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
                assert!(app.engine.show_lyrics_panel);
                assert_eq!(app.engine.lyrics_fullscreen, Some(was_fullscreen));
                app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
                assert_eq!(app.engine.lyrics_fullscreen, Some(was_fullscreen));
                app.apply(Action::SetLyricsFullscreen(false), ui.ctx());
                assert!(app.engine.show_lyrics_panel);
                assert_eq!(app.engine.lyrics_fullscreen, None);
            });
            output.textures_delta.clear();
            let commands: Vec<_> = output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .filter_map(|command| {
                    if let egui::ViewportCommand::Fullscreen(value) = command {
                        Some(*value)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(commands, vec![true, was_fullscreen]);
        }
    }

    #[test]
    fn native_fullscreen_exit_restores_the_previous_window_mode() {
        for was_fullscreen in [false, true] {
            let mut app = headless_app();
            app.engine.lyrics_fullscreen = Some(was_fullscreen);
            app.engine.lyrics_fullscreen_seen = true;
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            let mut input = egui::RawInput::default();
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .fullscreen = Some(false);
            let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
            output.textures_delta.clear();
            assert_eq!(app.engine.lyrics_fullscreen, None);
            assert!(!app.engine.lyrics_fullscreen_seen);
            let commands: Vec<_> = output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .filter_map(|command| {
                    if let egui::ViewportCommand::Fullscreen(value) = command {
                        Some(*value)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(commands, vec![was_fullscreen]);
        }
    }

    #[test]
    fn leaving_lyrics_fullscreen_keeps_window_bounds_until_native_restore() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        app.engine.lyrics_fullscreen = Some(false);
        app.engine.lyrics_fullscreen_seen = true;
        app.window_geometry.size = Some([900.0, 650.0]);
        app.window_geometry.position = Some([100.0, 80.0]);
        // Escape is applied while the viewport still reports the old screen.
        app.engine.actions.push(Action::SetLyricsFullscreen(false));
        for _ in 0..2 {
            let mut input = egui::RawInput::default();
            let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            viewport.fullscreen = Some(true);
            viewport.inner_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1920.0, 1080.0),
            ));
            viewport.outer_rect = viewport.inner_rect;
            let mut output = ctx.run_ui(input, |ui| {
                app.frame_ui(ui);
                app.remember_geometry(ui.ctx());
            });
            output.textures_delta.clear();
            assert_eq!(app.engine.lyrics_fullscreen, None);
            assert_eq!(app.window_geometry.size, Some([900.0, 650.0]));
            assert_eq!(app.window_geometry.position, Some([100.0, 80.0]));
        }
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.fullscreen = Some(false);
        viewport.inner_rect = Some(egui::Rect::from_min_size(
            egui::pos2(120.0, 100.0),
            egui::vec2(940.0, 680.0),
        ));
        viewport.outer_rect = viewport.inner_rect;
        let mut output = ctx.run_ui(input, |ui| {
            app.frame_ui(ui);
            app.remember_geometry(ui.ctx());
        });
        output.textures_delta.clear();
        assert_eq!(app.window_geometry.size, Some([940.0, 680.0]));
        assert_eq!(app.window_geometry.position, Some([120.0, 100.0]));
        app.engine.backend.shutdown();
    }

    #[test]
    fn reopening_lyrics_during_native_exit_keeps_the_original_window_mode() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.engine.lyrics_fullscreen = Some(false);
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .fullscreen = Some(true);
        let mut output = ctx.run_ui(input, |ui| {
            app.apply(Action::SetLyricsFullscreen(false), ui.ctx());
            app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
        });
        output.textures_delta.clear();
        assert_eq!(app.engine.lyrics_fullscreen, Some(false));
        assert_eq!(app.engine.lyrics_fullscreen_restoring, None);
        app.engine.backend.shutdown();
    }

    #[cfg(windows)]
    #[test]
    fn lyrics_fullscreen_clears_and_restores_windows_maximization() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.fullscreen = Some(false);
        viewport.maximized = Some(true);
        let mut output = ctx.run_ui(input, |ui| {
            app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
            app.apply(Action::SetLyricsFullscreen(false), ui.ctx());
        });
        output.textures_delta.clear();
        assert_eq!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .filter(|command| {
                    matches!(
                        command,
                        egui::ViewportCommand::Maximized(_) | egui::ViewportCommand::Fullscreen(_)
                    )
                })
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                egui::ViewportCommand::Maximized(false),
                egui::ViewportCommand::Fullscreen(true),
                egui::ViewportCommand::Fullscreen(false),
                egui::ViewportCommand::Maximized(true),
            ]
        );
        app.window_geometry.size = Some([900.0, 650.0]);
        // A native exit can report windowed before maximization catches up.
        // Do not save that intermediate size over the restored geometry.
        for maximized in [false, true] {
            let mut input = egui::RawInput::default();
            let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            viewport.fullscreen = Some(false);
            viewport.maximized = Some(maximized);
            viewport.inner_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 852.0),
            ));
            let mut output = ctx.run_ui(input, |ui| {
                app.frame_ui(ui);
                app.remember_geometry(ui.ctx());
            });
            output.textures_delta.clear();
            assert_eq!(app.engine.lyrics_fullscreen_restoring.is_none(), maximized);
            // Maximized keeps the normal size behind it, to restore to.
            assert_eq!(app.window_geometry.size, Some([900.0, 650.0]));
            assert_eq!(app.window_geometry.maximized, maximized);
        }
        app.engine.backend.shutdown();
    }

    #[cfg(feature = "demo")]
    #[test]
    fn escape_from_fullscreen_repositions_the_returning_lyrics_panel() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        crate::demo::apply_flags(&mut app, None, Some("lyrics"));
        app.engine.lyrics_fullscreen = Some(false);
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(760.0, 520.0),
            )),
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .fullscreen = Some(true);
        let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
        output.textures_delta.clear();
        assert_eq!(app.engine.lyrics_fullscreen, None);
        assert_eq!(
            app.engine.lyrics_line_shown, None,
            "the fullscreen line cannot stop the returning panel from positioning itself"
        );
        app.engine.backend.shutdown();
    }

    #[test]
    fn closing_lyrics_leaves_fullscreen() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
            app.apply(Action::ToggleLyricsPanel, ui.ctx());
        });
        output.textures_delta.clear();
        assert!(!app.engine.show_lyrics_panel);
        assert_eq!(app.engine.lyrics_fullscreen, None);
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Fullscreen(false))
        );
    }

    fn headless_app() -> App {
        let root =
            std::env::temp_dir().join(format!("spotifast-volume-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let mut app = App::new(
            &Waker::default(),
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.engine.local_ready = true;
        app
    }

    /// Dock menu picks wait in their own queue, which the application reads
    /// with or without a window, and never in the menu bar's, which only a
    /// window reads. A pick in the tray would otherwise wait for a window.
    #[cfg(target_os = "macos")]
    #[test]
    fn dock_menu_picks_become_playback_actions_without_a_window() {
        use crate::mac_menu::{self, MenuCommand};
        let mut app = headless_app();
        let _ = mac_menu::drain_dock_commands();
        mac_menu::push_dock_command(MenuCommand::PlayPause);
        mac_menu::push_dock_command(MenuCommand::Next);
        mac_menu::push_dock_command(MenuCommand::Previous);
        assert!(mac_menu::drain_commands().is_empty());
        app.engine.actions.clear();
        app.engine.handle_dock_menu();
        assert!(matches!(
            app.engine.actions.as_slice(),
            [Action::TogglePlay, Action::Next, Action::Previous]
        ));
        assert!(mac_menu::drain_dock_commands().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn notch_commands_become_playback_actions_without_a_window() {
        use crate::notch::{self, NotchCommand};
        let mut app = headless_app();
        let _ = notch::drain_commands();
        notch::push_command(NotchCommand::PlayPause);
        notch::push_command(NotchCommand::Next);
        notch::push_command(NotchCommand::Seek(45_000));
        app.engine.actions.clear();
        app.engine.handle_notch_commands();
        assert!(matches!(
            app.engine.actions.as_slice(),
            [Action::TogglePlay, Action::Next, Action::Seek(45_000)]
        ));
        assert!(notch::drain_commands().is_empty());
    }

    #[test]
    fn optimistic_local_seek_holds_position_until_player_confirms_seek() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:test1234".into(),
            title: "Test Track".into(),
            duration_ms: 180_000,
            ..Default::default()
        });
        app.engine.local.position_ms = 10_000;
        app.engine.local.playback = Playback::Playing;
        app.engine.local.seek_sequence = 1;

        assert!(app.engine.now_playing().is_some_and(|n| n.position_ms >= 10_000));

        app.engine.seek(95_000);
        assert_eq!(app.engine.pending_local_position.map(|(pos, _)| pos), Some(95_000));

        let now = app.engine.now_playing().expect("now playing");
        assert!(now.position_ms >= 95_000 && now.position_ms < 96_000);

        let mut stale_state = app.engine.local.clone();
        stale_state.position_ms = 11_000;
        app.handle_local(stale_state);
        assert_eq!(app.engine.pending_local_position.map(|(pos, _)| pos), Some(95_000));
        assert!(app.engine.now_playing().is_some_and(|n| n.position_ms >= 95_000));

        let mut confirmed_state = app.engine.local.clone();
        confirmed_state.seek_sequence = 2;
        confirmed_state.position_ms = 95_200;
        app.handle_local(confirmed_state);
        assert_eq!(app.engine.pending_local_position, None);
        assert_eq!(app.engine.now_playing().map(|n| n.position_ms), Some(95_200));
    }

    fn seed_playlist(app: &mut App, id: &str) {
        app.engine.playlist_pages
            .insert(id.to_string(), PlaylistPage::default());
        app.engine.open(Page::Playlist(id.to_string()));
        std::thread::sleep(Duration::from_millis(1));
    }

    fn store_track(app: &mut App, id: &str) {
        app.engine.track_cache.insert(
            id.to_string(),
            Track {
                id: Some(id.to_string()),
                uri: format!("spotify:track:{id}"),
                ..Track::default()
            },
        );
    }

    fn web_album(uri: &str, album_type: &str, album_group: Option<&str>) -> Album {
        Album {
            id: uri.rsplit(':').next().unwrap_or_default().into(),
            uri: uri.into(),
            album_type: Some(album_type.into()),
            album_group: album_group.map(str::to_string),
            ..Album::default()
        }
    }

    #[test]
    fn precise_album_types_are_requested_only_for_web_singles_and_once_per_uri() {
        use crate::api::models::{Page as ApiPage, SavedAlbum};

        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let first = web_album("spotify:album:first", "single", Some("single"));
        let second = web_album("spotify:album:second", "single", Some("single"));
        let detail = web_album("spotify:album:detail", "single", None);
        let album = web_album("spotify:album:album", "album", Some("album"));
        let compilation = web_album(
            "spotify:album:compilation",
            "compilation",
            Some("compilation"),
        );
        let appears_on = web_album("spotify:album:appears", "single", Some("appears_on"));

        app.handle_api(ApiResponse::SavedAlbums {
            offset: 0,
            result: Ok(ApiPage {
                items: vec![
                    SavedAlbum {
                        album: first.clone(),
                        ..SavedAlbum::default()
                    },
                    SavedAlbum {
                        album: compilation,
                        ..SavedAlbum::default()
                    },
                    SavedAlbum {
                        album,
                        ..SavedAlbum::default()
                    },
                ],
                ..ApiPage::default()
            }),
        });
        assert_eq!(
            app.engine.backend.take_album_type_requests(),
            vec![vec![first.uri.clone()]]
        );

        app.engine.artist_pages
            .insert("artist".into(), ArtistPage::default());
        app.handle_api(ApiResponse::ArtistAlbums {
            id: "artist".into(),
            groups: "album,single,compilation,appears_on".into(),
            offset: 0,
            result: Ok(ApiPage {
                items: vec![first, second.clone(), appears_on],
                ..ApiPage::default()
            }),
        });
        assert_eq!(
            app.engine.backend.take_album_type_requests(),
            vec![vec![second.uri.clone()]]
        );

        app.engine.album_pages
            .insert(detail.id.clone(), AlbumPage::default());
        app.handle_api(ApiResponse::Album {
            id: detail.id.clone(),
            result: Ok(detail.clone()),
        });
        assert_eq!(
            app.engine.backend.take_album_type_requests(),
            vec![vec![detail.uri.clone()]]
        );

        app.handle_api(ApiResponse::Album {
            id: detail.id.clone(),
            result: Ok(detail),
        });
        assert!(app.engine.backend.take_album_type_requests().is_empty());
    }

    #[test]
    fn precise_ep_confirmation_preserves_fallback_and_web_kind_precedence() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let ep = web_album("spotify:album:ep", "single", Some("single"));
        let failed = web_album("spotify:album:failed", "single", Some("single"));
        let timed_out = web_album("spotify:album:timeout", "single", Some("single"));
        let regular = web_album("spotify:album:regular", "single", Some("single"));

        app.engine.request_album_types([&ep, &failed, &timed_out, &regular]);
        let _ = app.engine.backend.take_album_type_requests();
        app.handle_backend_events(vec![
            Event::AlbumType {
                uri: ep.uri.clone(),
                result: Ok(true),
            },
            Event::AlbumType {
                uri: failed.uri.clone(),
                result: Err("unavailable".into()),
            },
            Event::AlbumType {
                uri: timed_out.uri.clone(),
                result: Err("album metadata timed out".into()),
            },
            Event::AlbumType {
                uri: regular.uri.clone(),
                result: Ok(false),
            },
        ]);

        assert_eq!(app.engine.album_kind_label(&ep), "EP");
        assert_eq!(app.engine.album_kind_label(&failed), "Single");
        assert_eq!(app.engine.album_kind_label(&timed_out), "Single");
        assert_eq!(app.engine.album_kind_label(&regular), "Single");
        let appears_on = web_album(&ep.uri, "single", Some("appears_on"));
        let compilation = web_album(&ep.uri, "single", Some("compilation"));
        let album = web_album(&ep.uri, "single", Some("album"));
        assert_eq!(app.engine.album_kind_label(&appears_on), "Appears On");
        assert_eq!(app.engine.album_kind_label(&compilation), "Compilation");
        assert_eq!(app.engine.album_kind_label(&album), "Album");

        app.engine.request_album_types([&failed, &timed_out]);
        assert!(
            app.engine.backend.take_album_type_requests().is_empty(),
            "failed lookups are terminal for this session"
        );

        app.engine.handle_auth(AuthStatus::SignedOut);
        assert_eq!(app.engine.album_kind_label(&ep), "Single");
        app.engine.handle_auth(AuthStatus::Connected {
            username: "next-session".into(),
        });
        app.engine.request_album_types([&ep]);
        assert_eq!(app.engine.backend.take_album_type_requests(), vec![vec![ep.uri]]);
    }

    #[test]
    fn two_toggle_play_actions_in_one_batch_return_to_playing() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            title: "A".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.refresh_frame_now();
        assert!(app.engine.now_playing().expect("playing").playing);
        app.engine.actions.push(Action::TogglePlay);
        app.engine.actions.push(Action::TogglePlay);
        app.apply_actions(&ctx);
        assert!(
            app.engine.now_playing().expect("playing").playing,
            "the second toggle must see the first pause, not the drawing snapshot"
        );
    }

    #[test]
    fn page_cache_stays_bounded_when_history_is_long() {
        let mut app = headless_app();
        for i in 0..20 {
            seed_playlist(&mut app, &format!("pl{i}"));
        }
        assert!(
            app.engine.playlist_pages.len() <= 12,
            "history must not protect more pages than the cap: {}",
            app.engine.playlist_pages.len()
        );
        assert!(
            app.engine.playlist_pages.contains_key("pl19"),
            "the open page stays"
        );
        assert!(
            !app.engine.playlist_pages.contains_key("pl0"),
            "the oldest page is dropped"
        );
    }

    #[test]
    fn navigating_past_the_cache_limit_keeps_edits_until_every_write_and_snapshot_is_confirmed() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "edited".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "edited".into(),
                    snapshot_id: Some("old".into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                ..Default::default()
            },
        );
        app.engine.open(Page::Playlist("edited".into()));
        for uri in ["spotify:track:second", "spotify:track:third"] {
            app.apply(
                Action::ConfirmAddToPlaylist {
                    position: None,
                    playlist_id: "edited".into(),
                    playlist_name: "Edited".into(),
                    items: vec![cached_playlist_row(uri).playable().unwrap().clone()],
                },
                &egui::Context::default(),
            );
        }
        assert_eq!(app.engine.playlist_pages["edited"].pending_writes, 2);
        for i in 0..25 {
            seed_playlist(&mut app, &format!("other-{i}"));
        }
        app.engine.open(Page::Playlist("edited".into()));
        let rows = |app: &App| {
            app.engine.playlist_pages["edited"]
                .items
                .items
                .iter()
                .filter_map(|item| item.playable().map(|item| item.uri().to_string()))
                .collect::<Vec<_>>()
        };
        let expected = [
            "spotify:track:first",
            "spotify:track:second",
            "spotify:track:third",
        ];
        assert_eq!(rows(&app), expected);
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Ok(Some("first-write".into())),
        });
        let generation = app.engine.playlist_pages["edited"].generation;
        let metadata = |snapshot: &str| ApiResponse::Playlist {
            id: "edited".into(),
            generation,
            result: Ok(Playlist {
                id: "edited".into(),
                snapshot_id: Some(snapshot.into()),
                items_count: Some(TrackCount { total: 3 }),
                ..Default::default()
            }),
        };
        app.handle_api(metadata("first-write"));
        assert_eq!(app.engine.playlist_pages["edited"].pending_writes, 1);
        assert_eq!(
            app.engine.playlist_pages["edited"].optimistic_snapshot.as_deref(),
            Some("first-write"),
            "metadata cannot confirm the page while another write is pending"
        );
        for i in 25..50 {
            seed_playlist(&mut app, &format!("other-{i}"));
        }
        assert_eq!(rows(&app), expected, "one write is still pending");
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Ok(Some("second-write".into())),
        });
        app.handle_api(metadata("first-write"));
        for i in 50..75 {
            seed_playlist(&mut app, &format!("other-{i}"));
        }
        app.engine.open(Page::Playlist("edited".into()));
        assert_eq!(
            rows(&app),
            expected,
            "lagging metadata cannot lose the edit"
        );
        app.handle_api(metadata("second-write"));
        for i in 75..100 {
            seed_playlist(&mut app, &format!("other-{i}"));
        }
        assert!(
            !app.engine.playlist_pages.contains_key("edited"),
            "confirmed edits stop protecting an old page"
        );
        assert!(app.engine.playlist_pages.len() <= 12);
    }

    #[test]
    fn transferring_back_takes_the_connect_session_without_replaying_a_snapshot() {
        for playing in [false, true] {
            for snapshot in ["fresh", "stale", "missing"] {
                let mut app = test_app(&format!("transfer-back-{playing}-{snapshot}"));
                app.engine.local_ready = true;
                app.engine.local_device_id = Some("this-computer".into());
                app.engine.local = LocalState {
                    connected: true,
                    playback: Playback::Stopped,
                    track: Some(crate::player::LocalTrack {
                        uri: "spotify:track:bellaire".into(),
                        ..Default::default()
                    }),
                    track_sequence: 3,
                    ..Default::default()
                };
                app.engine.selected_device = Some("phone".into());
                app.engine.remote = Some(RemoteSnapshot {
                    state: PlaybackState {
                        is_playing: playing,
                        progress_ms: Some(65_000),
                        item: Some(queued_song("spotify:track:metallica")),
                        device: Some(crate::api::models::Device {
                            id: Some("phone".into()),
                            is_active: true,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    received_at: Instant::now(),
                });
                app.on_now_playing_changed();
                let upcoming = [
                    "spotify:track:metallica",
                    "spotify:track:extra",
                    "spotify:track:context",
                ];
                app.engine.queue = loaded_queue("spotify:track:metallica", &upcoming);
                app.engine.manual_queue = upcoming[..2].iter().map(|uri| uri.to_string()).collect();
                // Old local restoration data must not be appended to the transfer.
                app.engine.resume_track = Some("spotify:track:metallica".into());
                app.engine.resume_queue = vec!["spotify:track:old-queue".into()];
                app.engine.local_list = Some(vec!["spotify:track:bellaire".into()]);
                match snapshot {
                    "stale" => {
                        app.engine.remote.as_mut().unwrap().received_at = Instant::now() - REMOTE_FRESH
                    }
                    "missing" => app.engine.remote = None,
                    _ => {}
                }
                app.engine.backend.take_player_commands();
                app.engine.transfer("this-computer".into());
                assert_eq!(
                    app.engine.backend.take_player_commands(),
                    [PlayerCommand::Transfer]
                );
                assert!(app.engine.local_list.is_none());
                assert!(app.engine.resume_queue.is_empty());
                assert_eq!(queue_uris(&app).1, upcoming);

                // Loading arrives before TrackChanged and still carries the
                // old local song. Keep showing the remote song through it.
                let loading = LocalState {
                    playback: Playback::Loading,
                    ..app.engine.local.clone()
                };
                app.handle_local(loading);
                if snapshot == "fresh" {
                    assert_eq!(app.engine.now_playing().unwrap().uri, "spotify:track:metallica");
                }
                assert_eq!(queue_uris(&app).1, upcoming);

                let transferred = LocalState {
                    connected: true,
                    playback: if playing {
                        Playback::Playing
                    } else {
                        Playback::Paused
                    },
                    track: Some(crate::player::LocalTrack {
                        uri: "spotify:track:metallica".into(),
                        duration_ms: 300_000,
                        ..Default::default()
                    }),
                    position_ms: 65_000,
                    track_sequence: 4,
                    ..Default::default()
                };
                app.handle_local(transferred.clone());
                let now = app.engine.now_playing().unwrap();
                assert_eq!(now.uri, "spotify:track:metallica");
                assert_eq!(now.position_ms, 65_000);
                assert_eq!(now.playing, playing);
                assert_eq!(app.engine.target(), Target::Local);
                assert_eq!(queue_uris(&app).1, upcoming);
                assert_eq!(
                    app.engine.manual_queue.len(),
                    2,
                    "handoff does not consume a queued copy"
                );
                assert!(app.engine.backend.take_queue_requests().is_empty());
                // Selecting this computer again must not suppress the next advance.
                app.engine.transfer("this-computer".into());
                assert!(app.engine.backend.take_player_commands().is_empty());
                app.handle_local(LocalState {
                    track_sequence: 5,
                    ..transferred
                });
                assert_eq!(queue_uris(&app).1, upcoming[1..]);
                assert_eq!(app.engine.manual_queue, ["spotify:track:extra"]);
                app.engine.backend.shutdown();
            }
        }
    }

    #[test]
    fn a_frame_snapshot_does_not_freeze_local_after_remote_handoff() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:local".into(),
            title: "Local".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        app.refresh_frame_now();
        assert_eq!(app.engine.now_playing().expect("local").uri, "spotify:track:local");
        app.engine.local.track = None;
        app.engine.local.playback = Playback::Stopped;
        app.engine.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                is_playing: true,
                item: Some(PlayableItem::Track(Track {
                    id: Some("remote".into()),
                    uri: "spotify:track:remote".into(),
                    name: "Remote".into(),
                    ..Track::default()
                })),
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        assert_eq!(
            app.engine.now_playing().expect("stale snapshot").uri,
            "spotify:track:local"
        );
        app.refresh_frame_now();
        assert_eq!(
            app.engine.now_playing().expect("handoff").uri,
            "spotify:track:remote"
        );
    }

    #[test]
    fn going_back_refreshes_page_recency() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        for i in 0..12 {
            seed_playlist(&mut app, &format!("pl{i}"));
        }
        for _ in 0..11 {
            app.apply(Action::Back, &ctx);
        }
        assert_eq!(app.engine.page(), &Page::Playlist("pl0".into()));
        seed_playlist(&mut app, "pl12");
        assert!(
            app.engine.playlist_pages.contains_key("pl0"),
            "a playlist revisited through Back must not be the oldest"
        );
        assert!(
            !app.engine.playlist_pages.contains_key("pl11"),
            "the page left behind by history is the one that goes"
        );
    }

    #[test]
    fn page_cache_trims_data_that_arrives_after_navigation() {
        let mut app = headless_app();
        for i in 0..12 {
            seed_playlist(&mut app, &format!("pl{i}"));
        }
        app.engine.open(Page::Playlist("current".into()));
        app.engine.playlist_pages
            .insert("current".into(), PlaylistPage::default());
        app.engine.playlist_pages
            .insert("late".into(), PlaylistPage::default());
        app.engine.evict_stale_pages();
        assert!(app.engine.playlist_pages.len() <= 12);
        assert!(app.engine.playlist_pages.contains_key("current"));
        assert!(
            !app.engine.playlist_pages.contains_key("late"),
            "a page that arrived after browsing still counts toward the cap"
        );
    }

    /// Playing music keeps the saved resume point fresh on its own, so a
    /// session that ends without a goodbye resumes close to where it was.
    #[test]
    fn playing_music_saves_the_resume_point_now_and_again() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        let ctx = egui::Context::default();
        let pass = |app: &mut App| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.background_frame(ui.ctx());
            });
            output.textures_delta.clear();
        };
        pass(&mut app);
        let saved = app.engine.last_session_save;

        pass(&mut app);
        assert_eq!(app.engine.last_session_save, saved, "no save while it is fresh");

        app.engine.last_session_save = Instant::now() - SESSION_REFRESH - Duration::from_secs(1);
        app.engine.session_dirty = false;
        pass(&mut app);
        assert!(!app.engine.session_dirty);
        assert!(
            app.engine.last_session_save.elapsed() < SESSION_REFRESH,
            "saved again"
        );
    }

    /// eframe runs only the logic of a hidden, minimised or occluded window,
    /// so the logic pass alone must ask for the next one. Otherwise a window
    /// on another workspace sleeps until something else wakes it, and stops
    /// polling the playing device and updating the media controls.
    #[test]
    fn logic_pass_alone_schedules_the_next_one_while_playing() {
        let mut app = headless_app();
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Playing;
        assert!(app.engine.now_playing().is_some_and(|now| now.playing));

        let ctx = egui::Context::default();
        let delays = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&delays);
        ctx.set_request_repaint_callback(move |info| {
            seen.lock().unwrap().push(info.delay);
        });
        // The first pass settles start-up work (zoom, fonts) that asks for an
        // immediate pass of its own; the steady state is what matters.
        for _ in 0..3 {
            delays.lock().unwrap().clear();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.background_frame(ui.ctx());
            });
            output.textures_delta.clear();
        }

        let soonest = delays.lock().unwrap().iter().copied().min();
        assert!(
            soonest
                .is_some_and(|delay| delay > Duration::ZERO && delay <= Duration::from_millis(250)),
            "a hidden window's logic must wake again within 250 ms while playing, \
             without spinning; asked for {soonest:?}"
        );
    }

    #[test]
    fn local_idle_repaint_is_twenty_seconds_not_four() {
        let mut app = headless_app();
        assert_eq!(app.connected_repaint_interval(), REMOTE_POLL_ACTIVE);
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Paused;
        assert_eq!(
            app.connected_repaint_interval(),
            REMOTE_POLL_IDLE,
            "paused local UI wait is 20s (already the API interval); 4s was only a tighter wake-up"
        );
    }

    /// Nobody can see the window and nothing plays: the poll slows to 30s.
    /// Music playing anywhere keeps it at its usual pace, for the media
    /// controls.
    #[test]
    fn an_unseen_idle_window_polls_less_often() {
        let mut app = headless_app();
        app.budget = crate::power::Budget::Background;
        assert_eq!(app.connected_repaint_interval(), REMOTE_POLL_UNSEEN);
        app.budget = crate::power::Budget::Saver;
        assert_eq!(app.connected_repaint_interval(), REMOTE_POLL_ACTIVE);
    }

    /// A poll asked for soon, after a transfer or a remote command, stays
    /// soon while nobody can see the window.
    #[test]
    fn a_poll_asked_for_soon_is_not_put_off_while_unseen() {
        let mut app = headless_app();
        app.budget = crate::power::Budget::Background;
        app.engine.poll_remote_soon();
        let due_in = app
            .remote_poll_interval()
            .0
            .saturating_sub(app.engine.remote_polled_at.elapsed());
        assert!(due_in <= Duration::from_millis(700), "due in {due_in:?}");
    }

    /// Playback polls, which spend the Web API quota, run at most every 15s
    /// even while the logic wakes more often.
    #[test]
    fn playback_polls_run_at_most_every_fifteen_seconds() {
        let mut app = headless_app();
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_MIN, false));
        app.budget = crate::power::Budget::Background;
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_UNSEEN, false));
        app.engine.selected_device = Some("phone".into());
        app.engine.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                is_playing: true,
                item: Some(queued_song("spotify:track:a")),
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        assert_eq!(app.connected_repaint_interval(), REMOTE_POLL_ACTIVE);
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_MIN, false));
        app.engine.selected_device = None;
        app.engine.remote = None;
        app.engine.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.engine.local.playback = Playback::Paused;
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_IDLE, false));
    }

    /// A command or switch another device accepted keeps the usual pace for a
    /// while, so a lagging answer is checked again before the optimistic hold
    /// ends. A refused command does not.
    #[test]
    fn a_remote_command_keeps_the_usual_poll_pace_for_a_while() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.selected_device = Some("phone".into());
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_MIN, false));

        app.handle_api(ApiResponse::Remote {
            action: RemoteAction::Pause,
            result: Err(crate::api::ApiError::RateLimited),
        });
        app.handle_api(ApiResponse::Transferred {
            device_id: "phone".into(),
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert!(app.engine.remote_command_at.is_none());

        app.handle_api(ApiResponse::Remote {
            action: RemoteAction::Pause,
            result: Ok(()),
        });
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_ACTIVE, false));
        app.engine.remote_polled_at = Instant::now() - Duration::from_millis(4_100);
        app.poll_remote_if_due();
        assert!(app.engine.remote_poll_pending, "polled at the usual pace");

        app.engine.remote_poll_pending = false;
        app.engine.remote_polled_at = Instant::now() - Duration::from_millis(4_100);
        app.engine.remote_command_at = Some(Instant::now() - REMOTE_COMMAND_WINDOW);
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_MIN, false));
        app.poll_remote_if_due();
        assert!(!app.engine.remote_poll_pending, "back to 15s once the window ends");

        app.handle_api(ApiResponse::Transferred {
            device_id: "phone".into(),
            result: Ok(()),
        });
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_ACTIVE, false));
    }

    /// A song on another device is polled again a second after it should
    /// end. An answer that still reports it, a moment from its end, does not
    /// bring another early poll within 15s.
    #[test]
    fn a_remote_song_end_is_polled_early_once() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.selected_device = Some("phone".into());
        let song = |progress_ms, received_at| RemoteSnapshot {
            state: PlaybackState {
                is_playing: true,
                progress_ms: Some(progress_ms),
                item: Some(crate::api::models::PlayableItem::Track(
                    crate::api::models::Track {
                        uri: "spotify:track:a".into(),
                        duration_ms: 180_000,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
            received_at,
        };
        let now = Instant::now();
        app.engine.remote_polled_at = now;
        app.engine.remote = Some(song(175_000, now));
        assert_eq!(app.remote_poll_interval(), (Duration::from_secs(6), true));

        let late = Instant::now() - Duration::from_secs(16);
        app.engine.remote_polled_at = late;
        app.engine.remote = Some(song(175_000, late));
        app.poll_remote_if_due();
        assert!(app.engine.remote_poll_pending, "the regular poll is due");
        assert!(
            app.engine.remote_end_polled_at.is_none(),
            "a late regular poll is not the early one"
        );
        app.engine.remote_poll_pending = false;

        let earlier = Instant::now() - Duration::from_secs(7);
        app.engine.remote_polled_at = earlier;
        app.engine.remote = Some(song(175_000, earlier));
        app.poll_remote_if_due();
        assert!(app.engine.remote_poll_pending, "the song's end is polled");
        assert!(app.engine.remote_end_polled_at.is_some());

        let earlier = Instant::now() - Duration::from_secs(2);
        app.engine.remote_poll_pending = false;
        app.engine.remote_polled_at = earlier;
        app.engine.remote = Some(song(179_500, earlier));
        app.poll_remote_if_due();
        assert!(!app.engine.remote_poll_pending, "no second early poll");
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_MIN, false));

        app.engine.remote_end_polled_at = None;
        app.engine.remote.as_mut().unwrap().state.is_playing = false;
        assert_eq!(app.remote_poll_interval(), (REMOTE_POLL_MIN, false));
    }

    /// Another device taking over stops this computer while the last poll
    /// still names it, so playback is polled again at once: otherwise Play
    /// would pull the music back from the other device.
    #[test]
    fn a_takeover_by_another_device_is_polled_at_once() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.local_ready = true;
        app.engine.local_device_id = Some("this-computer".into());
        let playing = LocalState {
            connected: true,
            playback: Playback::Playing,
            track: Some(crate::player::LocalTrack {
                uri: "spotify:track:a".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        app.engine.local = playing.clone();
        app.engine.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                is_playing: true,
                item: Some(queued_song("spotify:track:a")),
                device: Some(crate::api::models::Device {
                    id: Some("this-computer".into()),
                    is_active: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        app.engine.remote_polled_at = Instant::now();

        app.handle_local(LocalState {
            playback: Playback::Stopped,
            ..playing
        });
        assert!(matches!(app.engine.target(), Target::Local));
        app.poll_remote_if_due();
        assert!(app.engine.remote_poll_pending, "polled at once");
    }

    #[test]
    fn reset_data_drops_table_row_caches() {
        let mut app = headless_app();
        crate::ui::collection::cached_table_items(&mut app, Page::LikedSongs, 0, 0, 0, || {
            vec![(
                PlayableItem::Track(Track {
                    name: "Hold".into(),
                    uri: "spotify:track:hold".into(),
                    ..Track::default()
                }),
                None,
                None,
            )]
        });
        assert!(!app.engine.table_rows.is_empty());
        app.engine.reset_data();
        assert!(app.engine.table_rows.is_empty());
        crate::ui::collection::cached_table_items(&mut app, Page::LikedSongs, 0, 0, 0, || {
            vec![(
                PlayableItem::Track(Track {
                    name: "Fresh".into(),
                    uri: "spotify:track:fresh".into(),
                    ..Track::default()
                }),
                None,
                None,
            )]
        });
        let name = app.engine.table_rows[&Page::LikedSongs].items[0].0.name();
        assert_eq!(
            name, "Fresh",
            "reused revision 0 must not keep the old rows"
        );
    }

    #[test]
    fn track_cache_is_an_lru_of_eight_hundred() {
        let mut app = headless_app();
        for i in 0..900 {
            store_track(&mut app, &format!("t{i}"));
        }
        for i in 100..900 {
            let _ = app.engine.read_cached_track(&format!("t{i}"));
        }
        app.engine.evict_stale_pages();
        assert_eq!(app.engine.track_cache.len(), 800);
        assert!(
            app.engine.track_cache.contains_key("t899"),
            "a cache hit must keep that track"
        );
        assert!(!app.engine.track_cache.contains_key("t0"));
    }

    #[test]
    fn reset_data_clears_cache_recency() {
        let mut app = headless_app();
        seed_playlist(&mut app, "pl0");
        store_track(&mut app, "t0");
        let _ = app.engine.read_cached_track("t0");
        assert!(!app.engine.page_used.is_empty());
        assert!(!app.engine.track_used.is_empty());
        app.engine.reset_data();
        assert!(app.engine.page_used.is_empty());
        assert!(app.engine.track_used.is_empty());
    }

    #[test]
    fn stepping_resume_keeps_a_cached_preview() {
        use crate::api::models::{PlayableItem, PlaylistItem, Track};
        use crate::model::PagedList;
        let row = |uri: &str| PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                id: Some(uri.rsplit(':').next().unwrap().into()),
                uri: uri.into(),
                name: uri.into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let mut app = headless_app();
        let ctx = egui::Context::default();
        store_track(&mut app, "one");
        store_track(&mut app, "two");
        for i in 0..798 {
            store_track(&mut app, &format!("old{i}"));
            let _ = app.engine.read_cached_track(&format!("old{i}"));
        }
        app.engine.playlist_pages.insert(
            "pl1".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![row("spotify:track:one"), row("spotify:track:two")],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.engine.resume_context = Some("spotify:playlist:pl1".into());
        app.engine.resume_track = Some("spotify:track:one".into());
        app.apply(Action::Next, &ctx);
        assert_eq!(app.engine.resume_track.as_deref(), Some("spotify:track:two"));
        store_track(&mut app, "overflow");
        let _ = app.engine.read_cached_track("overflow");
        app.engine.evict_stale_pages();
        assert!(
            app.engine.track_cache.contains_key("two"),
            "a cache hit while stepping resume must keep the song just previewed"
        );
        assert!(
            !app.engine.track_cache.contains_key("one"),
            "the song left behind can go"
        );
    }

    #[test]
    fn requesting_a_cached_resume_track_keeps_it() {
        let mut app = headless_app();
        store_track(&mut app, "resume");
        store_track(&mut app, "decoy");
        for i in 0..798 {
            store_track(&mut app, &format!("old{i}"));
            let _ = app.engine.read_cached_track(&format!("old{i}"));
        }
        app.engine.resume_track = Some("spotify:track:resume".into());
        app.engine.request_resume_track();
        store_track(&mut app, "overflow");
        let _ = app.engine.read_cached_track("overflow");
        app.engine.evict_stale_pages();
        assert!(
            app.engine.track_cache.contains_key("resume"),
            "a cache hit on the resume URI must keep that track"
        );
        assert!(
            !app.engine.track_cache.contains_key("decoy"),
            "an untouched cached track can go"
        );
    }

    #[test]
    fn evict_stale_pages_drops_table_row_copies() {
        let mut app = headless_app();
        for i in 0..12 {
            seed_playlist(&mut app, &format!("pl{i}"));
        }
        crate::ui::collection::cached_table_items(
            &mut app,
            Page::Playlist("pl0".into()),
            0,
            0,
            0,
            || {
                vec![(
                    PlayableItem::Track(Track {
                        name: "Old".into(),
                        uri: "spotify:track:old".into(),
                        ..Track::default()
                    }),
                    None,
                    None,
                )]
            },
        );
        assert!(app.engine.table_rows.contains_key(&Page::Playlist("pl0".into())));
        seed_playlist(&mut app, "pl12");
        assert!(
            !app.engine.playlist_pages.contains_key("pl0"),
            "the oldest playlist page is dropped"
        );
        assert!(
            !app.engine.table_rows.contains_key(&Page::Playlist("pl0".into())),
            "its table-row copy must go with it"
        );
    }

    #[test]
    fn update_checks_leave_the_popup_closed_until_requested() {
        for manual in [false, true] {
            let mut app = headless_app();
            app.handle_backend_events(vec![Event::UpdateChecked {
                manual,
                result: Ok(Some(crate::updates::Release {
                    version: "1.2.3".into(),
                    url: "https://github.com/crmne/spotifast/releases/tag/v1.2.3".into(),
                })),
            }]);
            let ctx = egui::Context::default();
            app.apply_actions(&ctx);
            assert!(app.update.is_some());
            assert!(!app.show_update);
            app.apply(Action::ShowUpdate, &ctx);
            assert!(app.show_update);
        }
    }

    #[test]
    fn a_manual_update_check_reports_its_result() {
        let mut app = headless_app();
        app.update = Some(crate::updates::Release {
            version: "1.2.3".into(),
            url: "https://github.com/crmne/spotifast/releases/tag/v1.2.3".into(),
        });
        app.update_checking = true;
        app.handle_backend_events(vec![Event::UpdateChecked {
            manual: true,
            result: Ok(None),
        }]);

        assert!(!app.update_checking);
        assert_eq!(app.update, None);
        assert_eq!(
            app.engine.toasts.last().map(|toast| toast.message.as_str()),
            Some("Spotifast is up to date")
        );

        app.engine.toasts.clear();
        app.update_checking = true;
        app.handle_backend_events(vec![Event::UpdateChecked {
            manual: true,
            result: Err("GitHub is unavailable".into()),
        }]);

        assert!(!app.update_checking);
        assert_eq!(
            app.engine.toasts.last().map(|toast| toast.message.as_str()),
            Some("Couldn't check for updates: GitHub is unavailable")
        );
        assert_eq!(
            app.engine.toasts.last().map(|toast| &toast.kind),
            Some(&ToastKind::Error)
        );
    }

    #[test]
    fn the_daily_update_check_only_announces_a_new_release() {
        let mut app = headless_app();
        app.update_checking = true;
        app.handle_backend_events(vec![Event::UpdateChecked {
            manual: false,
            result: Ok(None),
        }]);
        assert!(
            app.engine.toasts.is_empty(),
            "the current release needs no daily toast"
        );

        app.update_checking = true;
        app.handle_backend_events(vec![Event::UpdateChecked {
            manual: false,
            result: Ok(Some(crate::updates::Release {
                version: "1.2.3".into(),
                url: "https://github.com/crmne/spotifast/releases/tag/v1.2.3".into(),
            })),
        }]);

        assert_eq!(
            app.update.as_ref().map(|release| release.version.as_str()),
            Some("1.2.3")
        );
        assert_eq!(
            app.engine.toasts.last().map(|toast| toast.message.as_str()),
            Some("Spotifast 1.2.3 is available")
        );
    }

    fn cached_playlist_row(uri: &str) -> crate::api::models::PlaylistItem {
        crate::api::models::PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                uri: uri.to_string(),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    #[test]
    fn playlist_row_cache_extends_without_cloning_old_rows_or_losing_null_slots() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "rows".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:first"),
                        Default::default(),
                    ],
                    total: Some(100),
                    next_offset: Some(2),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let cached = |app: &mut App| {
            let page = app.engine.playlist_pages.remove("rows").unwrap();
            let result = crate::ui::collection::playlist_cached_table_items(
                app,
                "rows",
                page.generation,
                &page.items,
                None,
                "",
            );
            app.engine.playlist_pages.insert("rows".into(), page);
            result
        };
        let (rows, positions, _) = cached(&mut app);
        assert_eq!(positions.as_slice(), [0]);
        let old_rows = Arc::as_ptr(&rows);
        let old_positions = Arc::as_ptr(&positions);
        drop(rows);
        drop(positions);

        app.handle_api(ApiResponse::PlaylistItems {
            id: "rows".into(),
            offset: 2,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![
                    cached_playlist_row("spotify:track:second"),
                    Default::default(),
                ],
                total: 100,
                limit: 2,
                offset: 2,
                next: Some("next".into()),
            }),
        });
        let (rows, positions, _) = cached(&mut app);
        assert_eq!(Arc::as_ptr(&rows), old_rows, "old rows were rebuilt");
        assert_eq!(Arc::as_ptr(&positions), old_positions);
        assert_eq!(positions.as_slice(), [0, 2]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].0.uri(), "spotify:track:second");
    }

    #[test]
    fn playlist_row_cache_rebuilds_for_edits_refreshes_and_window_jumps() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "rows".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(100),
                    next_offset: Some(1),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let cached = |app: &mut App, owner_name: &str| {
            let page = app.engine.playlist_pages.remove("rows").unwrap();
            let result = crate::ui::collection::playlist_cached_table_items(
                app,
                "rows",
                page.generation,
                &page.items,
                None,
                owner_name,
            );
            app.engine.playlist_pages.insert("rows".into(), page);
            result
        };
        let (rows, _, _) = cached(&mut app, "");
        let first = Arc::as_ptr(&rows);
        drop(rows);

        let (rows, _, _) = cached(&mut app, "renamed owner");
        assert_ne!(Arc::as_ptr(&rows), first);
        let first = Arc::as_ptr(&rows);
        drop(rows);

        let page = app.engine.playlist_pages.get_mut("rows").unwrap();
        page.items
            .items
            .insert(0, cached_playlist_row("spotify:track:added"));
        page.items.revision += 1;
        let (rows, positions, _) = cached(&mut app, "renamed owner");
        assert_ne!(Arc::as_ptr(&rows), first);
        assert_eq!(rows[0].0.uri(), "spotify:track:added");
        assert_eq!(positions.as_slice(), [0, 1]);
        drop(rows);
        drop(positions);

        app.handle_api(ApiResponse::PlaylistItems {
            id: "rows".into(),
            offset: 0,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:refreshed")],
                total: 100,
                limit: 1,
                offset: 0,
                next: Some("next".into()),
            }),
        });
        let (rows, positions, _) = cached(&mut app, "renamed owner");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.uri(), "spotify:track:refreshed");
        assert_eq!(positions.as_slice(), [0]);
        drop(rows);
        drop(positions);

        app.engine.playlist_pages
            .get_mut("rows")
            .unwrap()
            .items
            .window_request = Some(50);
        app.handle_api(ApiResponse::PlaylistItems {
            id: "rows".into(),
            offset: 50,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:window")],
                total: 100,
                limit: 1,
                offset: 50,
                next: Some("next".into()),
            }),
        });
        let (rows, positions, _) = cached(&mut app, "renamed owner");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.uri(), "spotify:track:window");
        assert_eq!(positions.as_slice(), [0]);
    }

    #[test]
    fn changing_playlist_items_keeps_the_library_visible() {
        let mut app = headless_app();
        app.engine.library.playlists = Loadable::Loaded(vec![
            Playlist {
                id: "changed".into(),
                snapshot_id: Some("old".into()),
                ..Default::default()
            },
            Playlist {
                id: "untouched".into(),
                snapshot_id: Some("same".into()),
                ..Default::default()
            },
        ]);
        app.engine.playlist_pages.insert(
            "changed".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "changed".into(),
                    snapshot_id: Some("old".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:held")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "changed".into(),
            message: "Added to Playlist".into(),
            result: Ok(Some("new".into())),
        });

        let playlists = app.engine.library
            .playlists
            .get()
            .expect("the loaded library stays on screen");
        assert_eq!(playlists.len(), 2);
        assert_eq!(playlists[0].snapshot_id.as_deref(), Some("new"));
        assert_eq!(playlists[1].snapshot_id.as_deref(), Some("same"));
        let open = app.engine.playlist_pages["changed"]
            .playlist
            .get()
            .expect("the playlist page");
        assert_eq!(open.snapshot_id.as_deref(), Some("new"));
        assert_eq!(
            app.engine.playlist_pages["changed"].items.items.len(),
            1,
            "a successful write does not throw away the loaded playlist"
        );
    }

    #[test]
    fn a_known_duplicate_opens_the_dialog_without_a_spotify_scan() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let honey = cached_playlist_row("spotify:track:honey");
        app.engine.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![honey.clone()],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        app.apply(
            Action::AddToPlaylist {
                playlist_id: "best".into(),
                playlist_name: "The best music ever".into(),
                items: vec![honey.playable().unwrap().clone()],
            },
            &egui::Context::default(),
        );

        assert!(!app.engine.playlist_busy, "there is no duplicate scan to wait for");
        assert!(matches!(
            app.engine.dialog,
            Some(Dialog::ConfirmPlaylistDuplicates {
                ref duplicate_uris,
                ..
            }) if duplicate_uris == &["spotify:track:honey"]
        ));
    }

    #[test]
    fn adding_to_a_loaded_playlist_is_immediate_and_keeps_its_cache() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.engine.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "best".into(),
            snapshot_id: Some("old".into()),
            items_count: Some(TrackCount { total: 1 }),
            ..Default::default()
        }]);
        app.engine.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                generation: 7,
                items_generation: 7,
                playlist: Loadable::Loaded(Playlist {
                    id: "best".into(),
                    snapshot_id: Some("old".into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:one")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                cache_saved_through: Some(1),
                ..Default::default()
            },
        );
        let added = cached_playlist_row("spotify:track:honey")
            .playable()
            .unwrap()
            .clone();

        app.apply(
            Action::ConfirmAddToPlaylist {
                position: None,
                playlist_id: "best".into(),
                playlist_name: "The best music ever".into(),
                items: vec![added],
            },
            &egui::Context::default(),
        );

        let page = &app.engine.playlist_pages["best"];
        assert_eq!(page.items.items.len(), 2, "the new row is shown at once");
        assert_eq!(page.items.total, Some(2));
        assert_eq!(page.items.next_offset, None);
        assert_eq!(
            page.items.items[0].playable().map(PlayableItem::uri),
            Some("spotify:track:one"),
            "the held rows are not discarded"
        );
        assert!(page.local_additions.contains("spotify:track:honey"));
        assert_eq!(page.cache_saved_through, None);

        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "best".into(),
            message: "Added to The best music ever".into(),
            result: Ok(Some("new".into())),
        });

        let page = &app.engine.playlist_pages["best"];
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(
            page.playlist
                .get()
                .and_then(|playlist| playlist.snapshot_id.as_deref()),
            Some("new")
        );
        assert_eq!(page.cache_saved_through, None);
        assert!(page.cache_write_pending.is_some());
        let generation = page.generation;
        app.engine.receive_playlist_cache_stored("alice", "best", generation, "new", true);
        assert_eq!(app.engine.playlist_pages["best"].cache_saved_through, Some(2));
        assert_eq!(app.engine.library.playlists.get().unwrap()[0].track_total(), 2);
    }

    #[test]
    fn confirmed_playlist_edit_waits_for_old_checkpoint_then_saves_new_snapshot() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.engine.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                generation: 7,
                items_generation: 7,
                playlist: Loadable::Loaded(Playlist {
                    id: "best".into(),
                    snapshot_id: Some("old".into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:one")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                ..Default::default()
            },
        );
        app.engine.checkpoint_playlist_cache("best");
        let old_generation = app.engine.playlist_pages["best"].generation;
        assert_eq!(
            app.engine.playlist_pages["best"]
                .cache_write_pending
                .as_ref()
                .map(|write| write.snapshot.as_str()),
            Some("old")
        );

        let added = cached_playlist_row("spotify:track:honey")
            .playable()
            .unwrap()
            .clone();
        app.apply(
            Action::ConfirmAddToPlaylist {
                position: None,
                playlist_id: "best".into(),
                playlist_name: "The best music ever".into(),
                items: vec![added],
            },
            &egui::Context::default(),
        );
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "best".into(),
            message: String::new(),
            result: Ok(Some("new".into())),
        });
        assert_eq!(
            app.engine.playlist_pages["best"]
                .cache_write_pending
                .as_ref()
                .map(|write| write.snapshot.as_str()),
            Some("old"),
            "the confirmed edit must wait for the older disk write"
        );

        app.engine.receive_playlist_cache_stored("alice", "best", old_generation, "old", true);
        let page = &app.engine.playlist_pages["best"];
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(page.cache_saved_through, None);
        assert_eq!(
            page.cache_write_pending
                .as_ref()
                .map(|write| write.snapshot.as_str()),
            Some("new"),
            "the stale completion must request the edited snapshot"
        );
        let new_generation = page.generation;
        app.engine.receive_playlist_cache_stored("alice", "best", new_generation, "new", true);
        assert_eq!(app.engine.playlist_pages["best"].cache_saved_through, Some(2));
    }

    #[test]
    fn positioned_playlist_additions_keep_partial_pages_and_ignore_old_reads() {
        for (base, position, scan_fails) in [
            (0, 1, false),
            (100, 101, false),
            (100, 102, true),
            (100, 50, false),
        ] {
            let mut app = headless_app();
            app.engine.backend.set_offline(true);
            app.engine.playlist_pages.insert(
                "target".into(),
                PlaylistPage {
                    generation: 9,
                    items_generation: 9,
                    items: PagedList {
                        base_offset: base,
                        items: vec![
                            cached_playlist_row("spotify:track:a"),
                            cached_playlist_row("spotify:track:b"),
                        ],
                        total: Some(200),
                        next_offset: Some(base + 2),
                        loaded_once: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            let item = cached_playlist_row("spotify:track:new")
                .playable()
                .unwrap()
                .clone();
            app.engine.request_playlist_add("target".into(), "Target".into(), vec![item], Some(position));
            let mut sent = app.engine.backend.take_playlist_add_requests();
            assert_eq!(sent.len(), 1);
            let ApiRequest::CheckPlaylistDuplicates {
                playlist_id,
                playlist_name,
                items,
                position: requested,
            } = sent.pop().unwrap()
            else {
                panic!("expected duplicate check");
            };
            assert_eq!(requested, Some(position));
            app.handle_api(ApiResponse::PlaylistDuplicatesChecked {
                playlist_id,
                playlist_name,
                items,
                position: requested,
                result: if scan_fails {
                    Err(crate::api::ApiError::Network("offline".into()))
                } else {
                    Ok(vec![])
                },
            });
            let page = &app.engine.playlist_pages["target"];
            assert_eq!(page.items.total, Some(201));
            assert_eq!(page.items.next_offset, Some(base + 3));
            assert_eq!(
                page.items.base_offset,
                if position < base { base + 1 } else { base }
            );
            if position >= base {
                assert_eq!(page.items.items.len(), 3);
                assert_eq!(
                    page.items.items[(position - base) as usize]
                        .playable()
                        .unwrap()
                        .uri(),
                    "spotify:track:new"
                );
            } else {
                assert_eq!(page.items.items.len(), 2);
            }
            let held: Vec<_> = page
                .items
                .items
                .iter()
                .map(|row| row.playable().unwrap().uri().to_string())
                .collect();
            let sent = app.engine.backend.take_playlist_add_requests();
            assert!(
                matches!(sent.as_slice(), [ApiRequest::AddToPlaylist { position: Some(at), .. }] if *at == position)
            );
            app.handle_api(ApiResponse::PlaylistItems {
                id: "target".into(),
                offset: base,
                generation: 9,
                result: Ok(crate::api::models::Page {
                    items: vec![cached_playlist_row("spotify:track:old")],
                    total: 200,
                    ..Default::default()
                }),
            });
            assert_eq!(
                app.engine.playlist_pages["target"]
                    .items
                    .items
                    .iter()
                    .map(|row| row.playable().unwrap().uri().to_string())
                    .collect::<Vec<_>>(),
                held
            );
            app.handle_api(ApiResponse::PlaylistItemsChanged {
                id: "target".into(),
                message: String::new(),
                result: Ok(Some("new-snapshot".into())),
            });
            assert_eq!(
                app.engine.playlist_pages["target"]
                    .items
                    .items
                    .iter()
                    .map(|row| row.playable().unwrap().uri().to_string())
                    .collect::<Vec<_>>(),
                held
            );
        }
    }

    #[test]
    fn duplicate_confirmation_keeps_the_requested_insertion_position() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "target".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:a"),
                        cached_playlist_row("spotify:track:b"),
                    ],
                    total: Some(2),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let item = cached_playlist_row("spotify:track:b")
            .playable()
            .unwrap()
            .clone();
        app.engine.request_playlist_add(
            "target".into(),
            "Target".into(),
            vec![item.clone()],
            Some(0),
        );
        assert!(app.engine.backend.take_playlist_add_requests().is_empty());
        app.apply(Action::CloseDialog, &egui::Context::default());
        assert_eq!(app.engine.playlist_pages["target"].items.items.len(), 2);
        app.engine.request_playlist_add("target".into(), "Target".into(), vec![item], Some(0));
        let Some(Dialog::ConfirmPlaylistDuplicates {
            playlist_id,
            playlist_name,
            items,
            position,
            ..
        }) = app.engine.dialog.take()
        else {
            panic!("duplicate confirmation");
        };
        assert_eq!(position, Some(0));
        app.apply(
            Action::ConfirmAddToPlaylist {
                playlist_id,
                playlist_name,
                items,
                position,
            },
            &egui::Context::default(),
        );
        assert_eq!(
            app.engine.playlist_pages["target"]
                .items
                .items
                .iter()
                .map(|row| row.playable().unwrap().uri())
                .collect::<Vec<_>>(),
            ["spotify:track:b", "spotify:track:a", "spotify:track:b"]
        );
        assert!(matches!(
            app.engine.backend.take_playlist_add_requests().as_slice(),
            [ApiRequest::AddToPlaylist {
                position: Some(0),
                ..
            }]
        ));
    }

    /// Copying picked songs puts their links on the clipboard, one per
    /// line. Pasting links into an editable playlist appends the songs:
    /// ones this app has shown get their rows at once, and the rest wait
    /// only for Spotify to name them. Links that are not songs are left out.
    #[test]
    fn copied_and_pasted_song_links_append_to_an_editable_playlist() {
        // #given an editable playlist with one song
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "target".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "target".into(),
                    name: "Target".into(),
                    collaborative: true,
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:old")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let track = |id: &str| Track {
            id: Some(id.into()),
            uri: format!("spotify:track:{id}"),
            name: format!("Song {id}"),
            ..Default::default()
        };
        let song = |id: &str| PlayableItem::Track(track(id));
        let rows = |app: &App| {
            app.engine.playlist_pages["target"]
                .items
                .items
                .iter()
                .map(|row| row.playable().unwrap().name().to_string())
                .collect::<Vec<_>>()
        };
        let ctx = egui::Context::default();

        // #when two songs are copied
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.apply(Action::CopySongs(vec![song("aaa"), song("bbb")]), ui.ctx());
        });
        output.textures_delta.clear();

        // #then their links are on the clipboard, one per line
        assert!(
            output
                .platform_output
                .commands
                .contains(&egui::OutputCommand::CopyText(
                    [
                        "https://open.spotify.com/track/aaa",
                        "https://open.spotify.com/track/bbb"
                    ]
                    .join(if cfg!(windows) { "\r\n" } else { "\n" })
                ))
        );

        // #when those links are pasted into the playlist
        app.apply(
            Action::PasteSongs {
                playlist_id: "target".into(),
                text: "https://open.spotify.com/track/aaa?si=x\r\nspotify:track:bbb\n".into(),
            },
            &ctx,
        );

        // #then their rows appear at once, named, and Spotify is asked to add them
        assert_eq!(rows(&app), ["", "Song aaa", "Song bbb"]);
        assert!(matches!(
            app.engine.backend.take_playlist_add_requests().as_slice(),
            [ApiRequest::AddToPlaylist { uris, position: None, .. }]
                if uris == &["spotify:track:aaa", "spotify:track:bbb"]
        ));
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "target".into(),
            message: String::new(),
            result: Ok(Some("after-paste".into())),
        });

        // #when links to songs this app has not seen are pasted, with an album
        app.apply(
            Action::PasteSongs {
                playlist_id: "target".into(),
                text: "spotify:track:ccc https://open.spotify.com/album/xyz, spotify:track:ddd"
                    .into(),
            },
            &ctx,
        );

        // #then Spotify is asked about the songs, and nothing is added yet
        assert!(app.engine.track_requests.contains("ccc"));
        assert!(app.engine.track_requests.contains("ddd"));
        assert_eq!(rows(&app).len(), 3);
        assert!(app.engine.backend.take_playlist_add_requests().is_empty());

        // #when Spotify names one song and has no other
        app.handle_api(ApiResponse::Track {
            id: "ccc".into(),
            result: Ok(track("ccc")),
        });
        assert!(app.engine.backend.take_playlist_add_requests().is_empty());
        app.handle_api(ApiResponse::Track {
            id: "ddd".into(),
            result: Err(crate::api::client::ApiError::Status {
                status: 404,
                message: "not found".into(),
            }),
        });

        // #then the named song is appended and the other is reported
        assert_eq!(rows(&app), ["", "Song aaa", "Song bbb", "Song ccc"]);
        assert!(matches!(
            app.engine.backend.take_playlist_add_requests().as_slice(),
            [ApiRequest::AddToPlaylist { uris, .. }] if uris == &["spotify:track:ccc"]
        ));
        assert!(
            app.engine.toasts
                .iter()
                .any(|toast| toast.message == "1 pasted link could not be added")
        );
        assert!(app.engine.pending_pastes.is_empty());

        // #when the clipboard holds no song links
        app.apply(
            Action::PasteSongs {
                playlist_id: "target".into(),
                text: "just some words".into(),
            },
            &ctx,
        );

        // #then nothing is added, and the user is told why
        assert!(app.engine.backend.take_playlist_add_requests().is_empty());
        assert!(
            app.engine.toasts
                .iter()
                .any(|toast| toast.message == "The clipboard has no Spotify song links")
        );

        // #when the playlist cannot be edited
        if let Loadable::Loaded(playlist) =
            &mut app.engine.playlist_pages.get_mut("target").unwrap().playlist
        {
            playlist.collaborative = false;
        }
        app.apply(
            Action::PasteSongs {
                playlist_id: "target".into(),
                text: "spotify:track:aaa".into(),
            },
            &ctx,
        );

        // #then nothing is added to it
        assert_eq!(rows(&app).len(), 4);
        assert!(app.engine.backend.take_playlist_add_requests().is_empty());
    }

    #[test]
    fn appending_before_any_playlist_page_is_loaded_keeps_the_first_page_offset() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages
            .insert("target".into(), PlaylistPage::default());
        app.engine.add_to_playlist_now(
            "target".into(),
            "Target".into(),
            vec![
                cached_playlist_row("spotify:track:new")
                    .playable()
                    .unwrap()
                    .clone(),
            ],
            None,
        );
        assert_eq!(app.engine.playlist_pages["target"].items.next_offset, Some(0));
        assert!(matches!(
            app.engine.backend.take_playlist_add_requests().as_slice(),
            [ApiRequest::AddToPlaylist { position: None, .. }]
        ));
    }

    #[test]
    fn playlist_refresh_preserves_the_view_and_failed_reads_keep_its_rows() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let page = Page::Playlist("best".into());
        app.engine.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(1),
                    loaded_once: true,
                    ..Default::default()
                },
                filter: "first".into(),
                ..Default::default()
            },
        );
        let sort = TableSort {
            column: SortColumn::Title,
            ascending: false,
        };
        app.engine.table_sorts.insert(page.clone(), sort);
        app.engine.pick_row(&page, "best", 0, RowPick::Only, 1);
        app.engine.reload(page.clone());
        let generation = app.engine.playlist_pages["best"].generation;
        assert_eq!(
            app.engine.backend.take_playlist_item_requests(),
            [("best".into(), 0, generation)]
        );
        assert_eq!(app.engine.table_sorts[&page], sort);
        assert_eq!(picked(&app, &page), [0]);
        assert_eq!(app.engine.playlist_pages["best"].filter, "first");
        app.handle_api(ApiResponse::PlaylistItems {
            id: "best".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::Network("offline".into())),
        });
        let playlist = &app.engine.playlist_pages["best"];
        assert_eq!(playlist.items.items.len(), 1);
        assert!(!playlist.items.loading);
        assert!(playlist.items.error.is_some());
        app.engine.reload(page);
        assert!(app.engine.playlist_pages["best"].items.loading);
        assert!(app.engine.playlist_pages["best"].items.error.is_none());
        assert_eq!(app.engine.backend.take_playlist_item_requests().len(), 1);
    }

    #[test]
    fn failed_playlist_write_finishes_a_waiting_refresh_with_the_recovery_read() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "edited".into(),
            PlaylistPage {
                pending_writes: 1,
                ..Default::default()
            },
        );
        app.engine.reload(Page::Playlist("edited".into()));
        assert!(app.engine.backend.take_playlist_item_requests().is_empty());
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Err(crate::api::ApiError::Network("offline".into())),
        });
        let generation = app.engine.playlist_pages["edited"].generation;
        assert_eq!(
            app.engine.backend.take_playlist_item_requests(),
            [("edited".into(), 0, generation)]
        );
        app.handle_api(ApiResponse::Playlist {
            id: "edited".into(),
            generation,
            result: Ok(Playlist::default()),
        });
        assert!(
            app.engine.backend.take_playlist_item_requests().is_empty(),
            "the recovery read already handles the waiting refresh"
        );
        app.handle_api(ApiResponse::PlaylistItems {
            id: "edited".into(),
            offset: 0,
            generation,
            result: Ok(crate::api::models::Page::default()),
        });
        assert!(!app.engine.playlist_pages["edited"].items.loading);
    }

    #[test]
    fn playlist_refresh_waits_for_all_writes_and_their_confirmed_snapshot() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "edited".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "edited".into(),
                    snapshot_id: Some("old".into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(1),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        for uri in ["spotify:track:second", "spotify:track:third"] {
            app.engine.add_to_playlist_now(
                "edited".into(),
                "Edited".into(),
                vec![cached_playlist_row(uri).playable().unwrap().clone()],
                None,
            );
        }
        let generation = app.engine.playlist_pages["edited"].generation;
        app.engine.reload(Page::Playlist("edited".into()));
        assert!(
            app.engine.backend.take_playlist_item_requests().is_empty(),
            "refresh must not read the rows before the pending writes finish"
        );
        let metadata = |snapshot: &str, total| ApiResponse::Playlist {
            id: "edited".into(),
            generation,
            result: Ok(Playlist {
                id: "edited".into(),
                snapshot_id: Some(snapshot.into()),
                items_count: Some(TrackCount { total }),
                ..Default::default()
            }),
        };
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Ok(Some("first-write".into())),
        });
        app.handle_api(metadata("first-write", 2));
        assert_eq!(app.engine.playlist_pages["edited"].items.total, Some(3));
        assert!(app.engine.backend.take_playlist_item_requests().is_empty());
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Ok(Some("second-write".into())),
        });
        app.handle_api(metadata("first-write", 2));
        assert_eq!(app.engine.playlist_pages["edited"].items.total, Some(3));
        assert!(app.engine.backend.take_playlist_item_requests().is_empty());
        assert!(app.engine.playlist_pages["edited"].items.loading);

        app.handle_api(metadata("second-write", 3));
        let refreshed_generation = app.engine.playlist_pages["edited"].generation;
        assert!(refreshed_generation > generation);
        assert_eq!(
            app.engine.backend.take_playlist_item_requests(),
            [("edited".into(), 0, refreshed_generation)]
        );
        let rows = |generation, uris: &[&str]| ApiResponse::PlaylistItems {
            id: "edited".into(),
            offset: 0,
            generation,
            result: Ok(crate::api::models::Page {
                total: uris.len() as u32,
                items: uris.iter().map(|uri| cached_playlist_row(uri)).collect(),
                ..Default::default()
            }),
        };
        app.handle_api(rows(generation, &["spotify:track:first"]));
        assert_eq!(app.engine.playlist_pages["edited"].items.items.len(), 3);
        assert!(app.engine.playlist_pages["edited"].items.loading);
        let expected = [
            "spotify:track:first",
            "spotify:track:second",
            "spotify:track:third",
            "spotify:track:added-elsewhere",
        ];
        app.handle_api(rows(refreshed_generation, &expected));
        let page = &app.engine.playlist_pages["edited"];
        assert!(!page.items.loading);
        assert_eq!(
            page.items
                .items
                .iter()
                .map(|row| row.playable().unwrap().uri())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn playlist_window_reads_wait_for_an_optimistic_snapshot_to_be_confirmed() {
        for unconfirmed in [false, true] {
            let mut app = headless_app();
            app.engine.backend.set_offline(true);
            app.engine.playlist_pages.insert(
                "edited".into(),
                PlaylistPage {
                    generation: 7,
                    items: PagedList {
                        items: (0..50)
                            .map(|index| {
                                cached_playlist_row(&format!("spotify:track:edited_{index}"))
                            })
                            .collect(),
                        total: Some(1_000),
                        next_offset: Some(50),
                        loaded_once: true,
                        ..Default::default()
                    },
                    optimistic_snapshot: unconfirmed.then(|| "written".into()),
                    ..Default::default()
                },
            );
            app.engine.load_window(Page::Playlist("edited".into()), 750);
            if unconfirmed {
                assert!(
                    app.engine.backend.take_playlist_item_requests().is_empty(),
                    "a distant read would describe the old playlist snapshot"
                );
                let rows = &app.engine.playlist_pages["edited"].items;
                assert_eq!(rows.base_offset, 0);
                assert_eq!(rows.items.len(), 50);
                assert_eq!(
                    rows.items[0].playable().unwrap().uri(),
                    "spotify:track:edited_0"
                );
                assert!(!rows.loading);
            } else {
                assert_eq!(
                    app.engine.backend.take_playlist_item_requests(),
                    [("edited".into(), 750, 7)]
                );
            }
            app.engine.backend.shutdown();
        }
    }

    #[test]
    fn playlist_refresh_can_retry_unconfirmed_edits_without_losing_them() {
        for (network_failure, manual_refresh, window_retry) in [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, false),
            (false, false, true),
            (false, true, true),
            (true, false, true),
            (true, true, true),
        ] {
            let mut app = headless_app();
            app.engine.backend.set_offline(true);
            app.engine.playlist_pages.insert(
                "edited".into(),
                PlaylistPage {
                    playlist: Loadable::Loaded(Playlist {
                        id: "edited".into(),
                        snapshot_id: Some("written".into()),
                        items_count: Some(TrackCount { total: 1 }),
                        ..Default::default()
                    }),
                    items: PagedList {
                        items: vec![cached_playlist_row("spotify:track:new")],
                        total: Some(1),
                        loaded_once: true,
                        ..Default::default()
                    },
                    optimistic_snapshot: Some("written".into()),
                    local_additions: ["spotify:track:new".into()].into(),
                    ..Default::default()
                },
            );
            if manual_refresh {
                app.engine.reload(Page::Playlist("edited".into()));
            }
            assert!(app.engine.backend.take_playlist_item_requests().is_empty());
            let generation = app.engine.playlist_pages["edited"].generation;
            let metadata = |snapshot: &str| ApiResponse::Playlist {
                id: "edited".into(),
                generation,
                result: Ok(Playlist {
                    snapshot_id: Some(snapshot.into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
            };
            if network_failure {
                app.handle_api(ApiResponse::Playlist {
                    id: "edited".into(),
                    generation,
                    result: Err(crate::api::ApiError::Network("offline".into())),
                });
            } else {
                for _ in 0..4 {
                    app.handle_api(metadata("old"));
                }
            }
            let page = &app.engine.playlist_pages["edited"];
            assert!(!page.items.loading, "failed refresh must stop spinning");
            assert!(page.items.error.is_some());
            assert_eq!(page.items.items.len(), 1);
            assert!(page.local_additions.contains("spotify:track:new"));
            assert_eq!(page.optimistic_snapshot.as_deref(), Some("written"));
            app.engine.load_more(Page::Playlist("edited".into()));
            assert!(app.engine.backend.take_playlist_item_requests().is_empty());

            let page = Page::Playlist("edited".into());
            app.apply(
                if window_retry {
                    Action::RetryWindow(page)
                } else {
                    Action::Reload(page)
                },
                &egui::Context::default(),
            );
            assert!(app.engine.backend.take_playlist_item_requests().is_empty());
            assert!(app.engine.playlist_pages["edited"].items.loading);
            assert!(app.engine.playlist_pages["edited"].items.error.is_none());
            app.handle_api(metadata("written"));
            assert_eq!(app.engine.backend.take_playlist_item_requests().len(), 1);
        }
    }

    #[test]
    fn stale_playlist_metadata_cannot_undo_a_successful_write() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                generation: 4,
                playlist: Loadable::Loaded(Playlist {
                    id: "best".into(),
                    snapshot_id: Some("before".into()),
                    items_count: Some(TrackCount { total: 2 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:one"),
                        cached_playlist_row("spotify:track:honey"),
                    ],
                    total: Some(2),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "best".into(),
            message: String::new(),
            result: Ok(Some("after".into())),
        });

        app.handle_api(ApiResponse::Playlist {
            id: "best".into(),
            generation: 4,
            result: Ok(Playlist {
                id: "best".into(),
                snapshot_id: Some("before".into()),
                items_count: Some(TrackCount { total: 1 }),
                ..Default::default()
            }),
        });

        let page = &app.engine.playlist_pages["best"];
        assert_eq!(
            page.playlist
                .get()
                .and_then(|playlist| playlist.snapshot_id.as_deref()),
            Some("after")
        );
        assert_eq!(page.items.total, Some(2));
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(page.snapshot_rechecks, 1);

        app.handle_api(ApiResponse::Playlist {
            id: "best".into(),
            generation: 4,
            result: Ok(Playlist {
                id: "best".into(),
                snapshot_id: Some("after".into()),
                items_count: Some(TrackCount { total: 2 }),
                ..Default::default()
            }),
        });

        let page = &app.engine.playlist_pages["best"];
        assert_eq!(page.optimistic_snapshot, None);
        assert_eq!(page.snapshot_rechecks, 0);
        assert_eq!(page.items.total, Some(2));
    }

    #[test]
    fn market_specific_track_uris_share_the_liked_state() {
        let mut app = headless_app();
        let recording = |uri: &str| Track {
            uri: uri.into(),
            external_ids: crate::api::models::ExternalIds {
                isrc: Some("GBUM71029604".into()),
            },
            ..Default::default()
        };
        let saved_uri = "spotify:track:original";
        let playing_uri = "spotify:track:playable";

        app.engine.remember_track_recording(&recording(saved_uri));
        app.engine.set_saved_state(saved_uri.into(), true);
        app.engine.remember_track_recording(&recording(playing_uri));

        assert_eq!(app.engine.is_saved(playing_uri), Some(true));
        assert_eq!(app.engine.saved_toggle_targets(playing_uri), vec![saved_uri]);
    }

    #[test]
    fn a_recording_follows_its_uris_when_they_move_or_change() {
        let mut app = headless_app();
        let recording = |uri: &str, isrc: &str| Track {
            uri: uri.into(),
            external_ids: crate::api::models::ExternalIds {
                isrc: Some(isrc.into()),
            },
            ..Default::default()
        };
        let (first, second) = ("spotify:track:first", "spotify:track:second");
        app.engine.remember_track_recording(&recording(first, "AAA"));
        app.engine.remember_track_recording(&recording(second, "AAA"));
        app.engine.set_saved_state(first.into(), true);
        assert_eq!(app.engine.is_saved(second), Some(true));
        // The liked URI turns out to be another recording: the old one
        // keeps nothing of it.
        app.engine.remember_track_recording(&recording(first, "BBB"));
        app.engine.set_saved_state(first.into(), true);
        app.engine.set_saved_state(second.into(), false);
        assert_eq!(app.engine.is_saved(second), Some(false));
        assert_eq!(app.engine.saved_toggle_targets(second), vec![second]);
        assert_eq!(app.engine.recording_uris["isrc:AAA"].len(), 1);
        // A URI marked saved directly still counts for its recording.
        app.engine.saved.insert(second.into(), true);
        app.engine.remember_track_recording(&recording("spotify:track:third", "AAA"));
        assert_eq!(app.engine.is_saved("spotify:track:third"), Some(true));
        app.engine.set_saved_state(first.into(), false);
        app.engine.remember_track_recording(&recording(first, "BBB"));
        assert_eq!(app.engine.is_saved(first), Some(false));
        app.engine.reset_data();
        assert!(app.engine.recording_uris.is_empty());
    }

    #[test]
    fn a_stale_contains_answer_does_not_undo_a_liked_song() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let uri = "spotify:track:honey";
        app.apply(Action::ToggleSaved(uri.into()), &egui::Context::default());
        assert_eq!(app.engine.is_saved(uri), Some(true));

        app.handle_api(ApiResponse::Contains {
            uris: vec![uri.into()],
            result: Ok(vec![false]),
        });

        assert_eq!(
            app.engine.is_saved(uri),
            Some(true),
            "the answer from before the click cannot make the heart flicker"
        );
    }

    #[test]
    fn adding_an_existing_song_asks_before_writing_it_again() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_busy = true;

        app.handle_api(ApiResponse::PlaylistDuplicatesChecked {
            position: None,
            playlist_id: "mix".into(),
            playlist_name: "Night mix".into(),
            items: vec![
                cached_playlist_row("spotify:track:again")
                    .playable()
                    .unwrap()
                    .clone(),
            ],
            result: Ok(vec!["spotify:track:again".into()]),
        });

        assert!(!app.engine.playlist_busy, "the confirmation is interactive");
        assert!(matches!(
            app.engine.dialog,
            Some(Dialog::ConfirmPlaylistDuplicates {
                ref playlist_id,
                ref duplicate_uris,
                ..
            }) if playlist_id == "mix" && duplicate_uris.len() == 1
        ));

        app.apply(
            Action::ConfirmAddToPlaylist {
                position: None,
                playlist_id: "mix".into(),
                playlist_name: "Night mix".into(),
                items: vec![
                    cached_playlist_row("spotify:track:again")
                        .playable()
                        .unwrap()
                        .clone(),
                ],
            },
            &egui::Context::default(),
        );

        assert!(app.engine.dialog.is_none());
        assert!(app.engine.playlist_busy, "the confirmed write is in flight");
    }

    #[test]
    fn a_song_not_in_the_playlist_needs_no_confirmation() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_busy = true;

        app.handle_api(ApiResponse::PlaylistDuplicatesChecked {
            position: None,
            playlist_id: "mix".into(),
            playlist_name: "Night mix".into(),
            items: vec![
                cached_playlist_row("spotify:track:new")
                    .playable()
                    .unwrap()
                    .clone(),
            ],
            result: Ok(Vec::new()),
        });

        assert!(app.engine.dialog.is_none());
        assert!(app.engine.playlist_busy, "the playlist write follows the check");
    }

    #[test]
    fn playlist_cache_count_must_match_spotify_before_it_can_choose_the_first_song() {
        for cache_first in [true, false] {
            let mut app = headless_app();
            app.engine.backend.set_offline(true);
            app.engine.user = Some(User {
                id: "alice".into(),
                ..Default::default()
            });
            app.engine.playlist_pages
                .insert("mix".into(), PlaylistPage::default());
            let header = ApiResponse::Playlist {
                id: "mix".into(),
                generation: 0,
                result: Ok(Playlist {
                    id: "mix".into(),
                    snapshot_id: Some("same-revision".into()),
                    items_count: Some(TrackCount { total: 3 }),
                    ..Default::default()
                }),
            };
            let cache = Some(PlaylistCache {
                snapshot: "same-revision".into(),
                items: vec![cached_playlist_row("spotify:track:wrong"); 4],
                total: 4,
                next_offset: None,
                appendable: false,
            });
            if cache_first {
                app.engine.receive_playlist_cache("alice", "mix", 0, cache);
                app.handle_api(header);
            } else {
                app.handle_api(header);
                app.engine.receive_playlist_cache("alice", "mix", 0, cache);
            }
            assert_eq!(
                app.engine.playlist_start("mix"),
                (None, Some(0)),
                "a matching revision cannot make a cache with the wrong count authoritative"
            );
            app.handle_api(ApiResponse::PlaylistItems {
                id: "mix".into(),
                offset: 0,
                generation: 0,
                result: Ok(crate::api::models::Page {
                    items: vec![cached_playlist_row("spotify:track:right"); 3],
                    total: 3,
                    limit: 50,
                    ..Default::default()
                }),
            });
            assert_eq!(
                app.engine.playlist_start("mix"),
                (Some("spotify:track:right".into()), None)
            );
            assert_eq!(app.engine.playlist_pages["mix"].items.items.len(), 3);
        }
    }

    #[test]
    fn playlist_cache_waits_until_all_optimistic_writes_are_confirmed() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.engine.playlist_pages.insert(
            "mix".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "mix".into(),
                    snapshot_id: Some("before".into()),
                    items_count: Some(TrackCount { total: 2 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:shown"); 2],
                    total: Some(2),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                pending_writes: 2,
                ..Default::default()
            },
        );
        app.engine.checkpoint_playlist_cache("mix");
        assert_eq!(app.engine.playlist_pages["mix"].cache_saved_through, None);
        for (snapshot, saved) in [("first-write", None), ("both-writes", None)] {
            app.handle_api(ApiResponse::PlaylistItemsChanged {
                id: "mix".into(),
                message: String::new(),
                result: Ok(Some(snapshot.into())),
            });
            assert_eq!(app.engine.playlist_pages["mix"].cache_saved_through, saved);
            assert_eq!(
                app.engine.playlist_pages["mix"].items.items.len(),
                2,
                "pending edits stay visible while disk persistence waits"
            );
        }
        assert!(app.engine.playlist_pages["mix"].cache_write_pending.is_some());
        app.engine.receive_playlist_cache_stored("alice", "mix", 0, "both-writes", true);
        assert_eq!(app.engine.playlist_pages["mix"].cache_saved_through, Some(2));
    }

    #[test]
    fn a_matching_partial_playlist_cache_resumes_at_its_next_page() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "large".into(),
            PlaylistPage {
                generation: 7,
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("current".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    loading: true,
                    ..Default::default()
                },
                pending_cache: Some(PlaylistCache {
                    snapshot: "current".into(),
                    items: vec![
                        cached_playlist_row("spotify:track:one"),
                        cached_playlist_row("spotify:track:two"),
                    ],
                    total: 10_000,
                    next_offset: Some(500),
                    appendable: true,
                }),
                cache_checked: true,
                ..Default::default()
            },
        );

        app.engine.try_adopt_playlist_cache("large");

        let page = &app.engine.playlist_pages["large"];
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(page.items.total, Some(10_000));
        assert_eq!(page.items.next_offset, Some(500));
        assert_eq!(page.cache_saved_through, Some(500));
        assert_eq!(page.cache_restored_through, Some(500));
        assert!(page.cache_append_valid);
        assert!(page.items.can_load_more());
        assert_eq!(
            app.engine.backend.take_playlist_sample_requests(),
            vec![("large".into(), 9_950, 7)],
            "restoring a prefix still samples the final page once"
        );

        // The offset-zero request starts alongside the cache read. Its late
        // answer must not replace the longer prefix that was restored.
        app.handle_api(ApiResponse::PlaylistItems {
            id: "large".into(),
            offset: 0,
            generation: 7,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:stale")],
                total: 10_000,
                limit: PLAYLIST_PAGE_SIZE,
                offset: 0,
                next: Some("next".into()),
            }),
        });
        let page = &app.engine.playlist_pages["large"];
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(
            page.items.items[0].playable().map(PlayableItem::uri),
            Some("spotify:track:one")
        );
        assert_eq!(page.items.next_offset, Some(500));

        // Reopening a valid incremental cache must not rewrite its prefix.
        app.engine.checkpoint_playlist_cache("large");
        assert!(app.engine.playlist_pages["large"].cache_write_pending.is_none());
        let page = app.engine.playlist_pages.get_mut("large").unwrap();
        page.items
            .items
            .push(cached_playlist_row("spotify:track:three"));
        page.items.next_offset = Some(1_000);
        app.engine.checkpoint_playlist_cache("large");
        assert!(
            app.engine.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| !pending.replacing)
        );
    }

    #[test]
    fn a_partial_playlist_cache_from_an_old_snapshot_is_not_shown() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "changed".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("new".into()),
                    ..Default::default()
                }),
                pending_cache: Some(PlaylistCache {
                    snapshot: "old".into(),
                    items: vec![cached_playlist_row("spotify:track:old")],
                    total: 10_000,
                    next_offset: Some(500),
                    appendable: false,
                }),
                cache_checked: true,
                ..Default::default()
            },
        );

        app.engine.try_adopt_playlist_cache("changed");

        let page = &app.engine.playlist_pages["changed"];
        assert!(page.items.items.is_empty());
        assert!(page.pending_cache.is_none());
        assert_eq!(page.cache_saved_through, None);
        assert_eq!(page.cache_restored_through, None);
    }

    #[test]
    fn playlist_cache_checkpoints_are_periodic_and_include_completion() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.engine.playlist_pages.insert(
            "large".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("current".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:one")],
                    total: Some(10_000),
                    next_offset: Some(50),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        app.engine.checkpoint_playlist_cache("large");
        assert_eq!(
            app.engine.playlist_pages["large"].cache_saved_through, None,
            "a short new prefix must not overwrite a longer cache before it is read"
        );
        app.engine.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .cache_checked = true;
        app.engine.checkpoint_playlist_cache("large");
        assert_eq!(app.engine.playlist_pages["large"].cache_saved_through, None);
        assert!(
            app.engine.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| pending.replacing)
        );
        app.engine.receive_playlist_cache_stored("alice", "large", 0, "current", true);
        assert_eq!(app.engine.playlist_pages["large"].cache_saved_through, Some(50));

        app.engine.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .items
            .next_offset = Some(100);
        app.engine.checkpoint_playlist_cache("large");
        assert_eq!(
            app.engine.playlist_pages["large"].cache_saved_through,
            Some(50),
            "one more page is too small to rewrite the growing cache"
        );

        app.engine.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .items
            .items
            .push(cached_playlist_row("spotify:track:two"));
        app.engine.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .items
            .next_offset = Some(550);
        app.engine.checkpoint_playlist_cache("large");
        assert_eq!(app.engine.playlist_pages["large"].cache_saved_through, Some(50));
        assert!(
            app.engine.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| !pending.replacing)
        );
        app.engine.receive_playlist_cache_stored("alice", "large", 0, "current", true);
        assert_eq!(app.engine.playlist_pages["large"].cache_saved_through, Some(550));
        assert_eq!(app.engine.playlist_pages["large"].cache_saved_rows, 2);

        let page = app.engine.playlist_pages.get_mut("large").expect("the playlist");
        page.items.total = Some(575);
        page.items.next_offset = None;
        app.engine.checkpoint_playlist_cache("large");
        assert_eq!(app.engine.playlist_pages["large"].cache_saved_through, Some(550));
        assert!(
            app.engine.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| pending.replacing)
        );
        app.engine.receive_playlist_cache_stored("alice", "large", 0, "current", true);
        assert_eq!(
            app.engine.playlist_pages["large"].cache_saved_through,
            Some(575),
            "the final short interval is still saved"
        );
    }

    #[test]
    fn playlist_cache_waits_for_the_previous_playlist_write() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        for id in ["first", "second"] {
            app.engine.playlist_pages.insert(
                id.into(),
                PlaylistPage {
                    playlist: Loadable::Loaded(Playlist {
                        id: id.into(),
                        snapshot_id: Some("current".into()),
                        ..Default::default()
                    }),
                    items: PagedList {
                        items: vec![cached_playlist_row("spotify:track:one")],
                        total: Some(1),
                        next_offset: None,
                        loaded_once: true,
                        ..Default::default()
                    },
                    cache_checked: true,
                    ..Default::default()
                },
            );
        }

        app.engine.checkpoint_playlist_cache("first");
        app.engine.checkpoint_playlist_cache("second");
        assert!(app.engine.playlist_pages["first"].cache_write_pending.is_some());
        assert!(app.engine.playlist_pages["second"].cache_write_pending.is_none());

        // A page may be evicted while its disk write is still in progress.
        app.engine.playlist_pages.remove("first");
        app.engine.receive_playlist_cache_stored("alice", "first", 0, "current", true);
        assert!(app.engine.playlist_pages["second"].cache_write_pending.is_some());
        app.engine.receive_playlist_cache_stored("alice", "second", 0, "current", true);
        assert_eq!(app.engine.playlist_pages["second"].cache_saved_through, Some(1));
    }

    #[test]
    fn stale_cache_write_completion_checkpoints_rows_after_playlist_resets() {
        #[derive(Clone, Copy, Debug)]
        enum Reset {
            WindowJump,
            Reload,
            SnapshotChange,
        }

        for reset in [Reset::WindowJump, Reset::Reload, Reset::SnapshotChange] {
            let mut app = headless_app();
            app.engine.backend.set_offline(true);
            app.engine.user = Some(User {
                id: "alice".into(),
                ..Default::default()
            });
            let old_generation = 7;
            app.engine.load_generation = old_generation;
            app.engine.playlist_pages.insert(
                "best".into(),
                PlaylistPage {
                    generation: old_generation,
                    items_generation: old_generation,
                    playlist: Loadable::Loaded(Playlist {
                        id: "best".into(),
                        snapshot_id: Some("old".into()),
                        items_count: Some(TrackCount { total: 1 }),
                        ..Default::default()
                    }),
                    items: PagedList {
                        items: vec![cached_playlist_row("spotify:track:old")],
                        total: Some(1),
                        next_offset: None,
                        loaded_once: true,
                        ..Default::default()
                    },
                    cache_checked: true,
                    ..Default::default()
                },
            );

            // Start an old-snapshot write, then invalidate its displayed rows.
            app.engine.checkpoint_playlist_cache("best");
            assert!(app.engine.playlist_cache_write_in_flight);
            assert_eq!(
                app.engine.playlist_pages["best"]
                    .cache_write_pending
                    .as_ref()
                    .map(|pending| pending.snapshot.as_str()),
                Some("old")
            );

            let current_snapshot = match reset {
                Reset::WindowJump => {
                    app.engine.load_playlist_items_at("best", 0);
                    "old"
                }
                Reset::Reload => {
                    app.engine.reload(Page::Playlist("best".into()));
                    let generation = app.engine.playlist_pages["best"].generation;
                    app.handle_api(ApiResponse::Playlist {
                        id: "best".into(),
                        generation,
                        result: Ok(Playlist {
                            id: "best".into(),
                            snapshot_id: Some("old".into()),
                            items_count: Some(TrackCount { total: 1 }),
                            ..Default::default()
                        }),
                    });
                    "old"
                }
                Reset::SnapshotChange => {
                    app.handle_api(ApiResponse::Playlist {
                        id: "best".into(),
                        generation: old_generation,
                        result: Ok(Playlist {
                            id: "best".into(),
                            snapshot_id: Some("new".into()),
                            items_count: Some(TrackCount { total: 1 }),
                            ..Default::default()
                        }),
                    });
                    "new"
                }
            };
            let generation = app.engine.playlist_pages["best"].generation;

            // The refreshed rows arrive while the old disk write still runs.
            app.handle_api(ApiResponse::PlaylistItems {
                id: "best".into(),
                offset: 0,
                generation,
                result: Ok(ApiPage {
                    items: vec![cached_playlist_row("spotify:track:new")],
                    total: 1,
                    limit: PLAYLIST_PAGE_SIZE,
                    offset: 0,
                    next: None,
                }),
            });
            assert_eq!(
                app.engine.playlist_pages["best"]
                    .cache_write_pending
                    .as_ref()
                    .map(|pending| pending.snapshot.as_str()),
                Some("old"),
                "{reset:?} must retain the in-flight write marker"
            );

            app.engine.receive_playlist_cache_stored("alice", "best", old_generation, "old", true);
            let pending = app.engine.playlist_pages["best"]
                .cache_write_pending
                .as_ref()
                .unwrap_or_else(|| panic!("{reset:?} lost its refreshed checkpoint"));
            assert_eq!(pending.generation, generation, "{reset:?}");
            assert_eq!(pending.snapshot, current_snapshot, "{reset:?}");
            assert!(app.engine.playlist_cache_write_in_flight, "{reset:?}");
        }
    }

    #[test]
    fn failed_playlist_cache_append_rebuilds_from_the_last_confirmed_checkpoint() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.engine.playlist_pages.insert(
            "large".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("current".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:one"),
                        cached_playlist_row("spotify:track:two"),
                    ],
                    total: Some(1_000),
                    next_offset: Some(550),
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                cache_saved_through: Some(50),
                cache_saved_rows: 1,
                cache_saved_total: Some(1_000),
                cache_append_valid: true,
                ..Default::default()
            },
        );

        app.engine.checkpoint_playlist_cache("large");
        assert!(
            app.engine.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| !pending.replacing)
        );
        app.engine.receive_playlist_cache_stored("alice", "large", 0, "current", false);
        assert_eq!(app.engine.playlist_pages["large"].cache_saved_through, Some(50));
        assert!(
            app.engine.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| pending.replacing)
        );
        app.engine.receive_playlist_cache_stored("alice", "large", 0, "current", true);
        assert_eq!(app.engine.playlist_pages["large"].cache_saved_through, Some(550));
        assert_eq!(app.engine.playlist_pages["large"].cache_saved_rows, 2);
    }

    #[test]
    fn removing_a_song_updates_the_finite_extent_immediately() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let mut items = PagedList::default();
        items.restore_cached(
            vec![
                cached_playlist_row("spotify:track:remove"),
                cached_playlist_row("spotify:track:keep"),
            ],
            2,
            None,
        );
        app.engine.playlist_pages.insert(
            "edit".into(),
            PlaylistPage {
                items,
                playlist: Loadable::Loaded(Playlist {
                    items_count: Some(TrackCount { total: 2 }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        app.apply(
            Action::RemoveFromPlaylist {
                playlist_id: "edit".into(),
                uris: vec!["spotify:track:remove".into()],
            },
            &egui::Context::default(),
        );
        let page = &app.engine.playlist_pages["edit"];
        assert_eq!(page.items.total, Some(1));
        assert_eq!(page.items.items.len(), 1);
        assert_eq!(page.items.next_offset, None);
        assert_eq!(
            page.playlist
                .get()
                .unwrap()
                .items_count
                .as_ref()
                .unwrap()
                .total,
            1
        );
    }

    #[test]
    fn an_overlapping_window_can_extend_a_restored_partial_prefix() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let mut items = PagedList::default();
        items.restore_cached(
            vec![cached_playlist_row("spotify:track:cached"); 499],
            1000,
            Some(499),
        );
        app.engine.playlist_pages.insert(
            "overlap".into(),
            PlaylistPage {
                items,
                cache_restored_through: Some(499),
                tail_checked: true,
                ..Default::default()
            },
        );
        app.engine.load_window(Page::Playlist("overlap".into()), 499);
        app.handle_api(ApiResponse::PlaylistItems {
            id: "overlap".into(),
            offset: 450,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:fresh"); 50],
                total: 1000,
                offset: 450,
                limit: 50,
                next: Some("next".into()),
            }),
        });
        let items = &app.engine.playlist_pages["overlap"].items;
        assert!(!items.loading);
        assert_eq!(items.items.len(), 500);
        assert_eq!(items.next_offset, Some(500));
    }

    #[test]
    fn late_disk_prefix_does_not_replace_a_pending_distant_window() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let mut items = PagedList::default();
        items.restore_cached(
            vec![cached_playlist_row("spotify:track:first"); 50],
            1000,
            Some(50),
        );
        items.window_at(720, 50);
        app.engine.playlist_pages.insert(
            "late".into(),
            PlaylistPage {
                generation: 7,
                items_generation: 7,
                tail_checked: true,
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("same".into()),
                    ..Default::default()
                }),
                items,
                pending_cache: Some(PlaylistCache {
                    snapshot: "same".into(),
                    items: vec![cached_playlist_row("spotify:track:cached"); 500],
                    total: 1000,
                    next_offset: Some(500),
                    appendable: false,
                }),
                ..Default::default()
            },
        );
        app.engine.try_adopt_playlist_cache("late");
        let page = &app.engine.playlist_pages["late"];
        assert_eq!(page.items.base_offset, 700);
        assert!(page.items.loading);
        assert_eq!(page.items.windows.get(&0).map(Vec::len), Some(500));
    }

    #[test]
    fn scrollbar_windows_share_the_cache_and_deduplicate_pending_requests() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let first = cached_playlist_row("spotify:track:first");
        app.engine.playlist_pages.insert(
            "scroll".into(),
            PlaylistPage {
                generation: 7,
                items_generation: 7,
                tail_checked: true,
                cache_checked: true,
                items: PagedList {
                    items: vec![first; 50],
                    total: Some(1000),
                    next_offset: Some(50),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.engine.load_window(Page::Playlist("scroll".into()), 720);
        app.engine.load_window(Page::Playlist("scroll".into()), 730);
        assert_eq!(
            app.engine.backend.take_playlist_item_requests(),
            vec![("scroll".into(), 700, 7)]
        );
        app.handle_api(ApiResponse::PlaylistItems {
            id: "scroll".into(),
            offset: 700,
            generation: 7,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:distant"); 50],
                total: 1000,
                offset: 700,
                limit: 50,
                next: Some("next".into()),
            }),
        });
        app.engine.load_window(Page::Playlist("scroll".into()), 10);
        assert!(app.engine.backend.take_playlist_item_requests().is_empty());
        assert_eq!(app.engine.playlist_pages["scroll"].items.base_offset, 0);
        assert_eq!(app.engine.playlist_pages["scroll"].items.total, Some(1000));
    }

    #[test]
    fn sorting_an_album_invalidates_the_in_flight_window() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.load_generation = 7;
        app.engine.album_pages.insert(
            "album".into(),
            AlbumPage {
                generation: 7,
                tracks: PagedList {
                    base_offset: 150,
                    total: Some(200),
                    loading: true,
                    window_request: Some(150),
                    next_offset: Some(150),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.engine.table_sorts.insert(
            Page::Album("album".into()),
            TableSort {
                column: SortColumn::Title,
                ascending: true,
            },
        );
        app.engine.load_more(Page::Album("album".into()));
        let generation = app.engine.album_pages["album"].generation;
        app.handle_api(ApiResponse::AlbumTracks {
            id: "album".into(),
            offset: 0,
            generation,
            result: Ok(crate::api::models::Page {
                items: vec![Track::default(); 50],
                total: 200,
                offset: 0,
                limit: 50,
                next: Some("next".into()),
            }),
        });
        app.handle_api(ApiResponse::AlbumTracks {
            id: "album".into(),
            offset: 150,
            generation: 7,
            result: Ok(crate::api::models::Page {
                items: vec![Track::default(); 50],
                total: 200,
                offset: 150,
                limit: 50,
                next: None,
            }),
        });
        assert_eq!(app.engine.album_pages["album"].tracks.items.len(), 50);
        assert_eq!(app.engine.album_pages["album"].tracks.next_offset, Some(50));
    }

    #[test]
    fn null_album_slots_are_not_shuffle_candidates() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let album: crate::api::models::Album = serde_json::from_str(r#"{"tracks":{"items":[{"uri":"spotify:track:a"},null,{"uri":"spotify:track:c"}],"total":3}}"#).unwrap();
        let mut page = AlbumPage::default();
        page.tracks.absorb(0, album.tracks.unwrap());
        app.engine.album_pages.insert("album".into(), page);
        assert_eq!(
            app.engine.context_track_uris("spotify:album:album"),
            Some(vec!["spotify:track:a".into(), "spotify:track:c".into()])
        );
    }

    #[test]
    fn album_window_from_an_evicted_generation_cannot_replace_current_rows() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.album_pages.insert(
            "album".into(),
            AlbumPage {
                generation: 8,
                ..Default::default()
            },
        );
        app.handle_api(ApiResponse::AlbumTracks {
            id: "album".into(),
            offset: 150,
            generation: 7,
            result: Ok(crate::api::models::Page {
                items: vec![Track::default()],
                total: 200,
                ..Default::default()
            }),
        });
        assert!(app.engine.album_pages["album"].tracks.items.is_empty());
    }

    #[test]
    fn a_distant_playlist_position_needs_one_direct_request() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.playlist_pages.insert(
            "large".into(),
            PlaylistPage {
                generation: 3,
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("current".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(10_000),
                    next_offset: Some(50),
                    loaded_once: true,
                    ..Default::default()
                },
                items_generation: 3,
                cache_checked: true,
                tail_checked: true,
                ..Default::default()
            },
        );
        let _ = app.engine.backend.take_playlist_item_requests();

        // Dragging the scrollbar far down asks for that window directly.
        app.apply(
            Action::LoadWindow {
                page: Page::Playlist("large".into()),
                position: 6_906,
            },
            &egui::Context::default(),
        );

        let requests = app.engine.backend.take_playlist_item_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "large");
        assert_eq!(requests[0].1, 6_900);
        let generation = requests[0].2;
        let page = &app.engine.playlist_pages["large"];
        assert_eq!(page.generation, generation);
        assert_eq!(page.items.base_offset, 6_900);
        assert_eq!(page.items.next_offset, Some(6_900));
        assert!(page.items.items.is_empty());

        app.handle_api(ApiResponse::PlaylistItems {
            id: "large".into(),
            offset: 6_900,
            generation,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:6901")],
                total: 10_000,
                limit: PLAYLIST_PAGE_SIZE,
                offset: 6_900,
                next: Some("next".into()),
            }),
        });
        let page = &app.engine.playlist_pages["large"];
        assert_eq!(page.items.base_offset, 6_900);
        assert_eq!(page.items.items.len(), 1);
        assert_eq!(page.items_generation, generation);

        // Filtering keeps its existing whole-list meaning. It restarts from
        // the beginning instead of pretending the distant window is all.
        app.engine.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .filter = "track".into();
        let _ = app.engine.backend.take_playlist_item_requests();
        app.engine.load_more(Page::Playlist("large".into()));
        let requests = app.engine.backend.take_playlist_item_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1, 0);
        assert_eq!(app.engine.playlist_pages["large"].items.base_offset, 0);
    }

    #[test]
    fn refresh_metadata_cannot_cache_rows_from_the_previous_generation() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.engine.playlist_pages.insert(
            "changed".into(),
            PlaylistPage {
                generation: 9,
                items_generation: 8,
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("old".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:old")],
                    total: Some(500),
                    next_offset: Some(50),
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                tail_checked: true,
                ..Default::default()
            },
        );

        app.handle_api(ApiResponse::Playlist {
            id: "changed".into(),
            generation: 9,
            result: Ok(Playlist {
                snapshot_id: Some("new".into()),
                ..Default::default()
            }),
        });

        let page = &app.engine.playlist_pages["changed"];
        assert_eq!(page.cache_saved_through, None);
        assert_eq!(page.items_generation, 8);
        assert_eq!(
            page.items.items[0].playable().map(PlayableItem::uri),
            Some("spotify:track:old")
        );

        app.handle_api(ApiResponse::PlaylistItems {
            id: "changed".into(),
            offset: 0,
            generation: 9,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:new")],
                total: 500,
                limit: PLAYLIST_PAGE_SIZE,
                offset: 0,
                next: Some("next".into()),
            }),
        });
        let page = &app.engine.playlist_pages["changed"];
        assert_eq!(page.items_generation, 9);
        assert_eq!(page.cache_saved_through, None);
        assert!(page.cache_write_pending.is_some());
        app.engine.receive_playlist_cache_stored("alice", "changed", 9, "new", true);
        let page = &app.engine.playlist_pages["changed"];
        assert_eq!(page.cache_saved_through, Some(50));
        assert_eq!(
            page.items.items[0].playable().map(PlayableItem::uri),
            Some("spotify:track:new")
        );
    }

    #[test]
    fn playlist_play_button_starts_at_the_first_row_instead_of_the_resume_track() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, Role, TreeId};

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.engine.remote = None;
        app.engine.selected_device = None;
        app.engine.local.connected = false;
        app.engine.shuffle_wanted = false;
        let context = "spotify:playlist:pl1";
        let rows = app.engine.context_track_uris(context).unwrap();
        app.engine.resume_context = Some(context.into());
        app.engine.resume_track = Some(rows[7].clone());
        app.engine.resume_position_ms = 23_000;
        app.engine.open(Page::Playlist("pl1".into()));
        assert!(matches!(app.engine.target(), Target::Local));

        let mut draw = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| crate::ui::collection::playlist(&mut app, ui, "pl1"),
            );
            output.textures_delta.clear();
            app.apply_actions(&ctx);
            output.platform_output.accesskit_update.unwrap()
        };
        draw(Vec::new());
        let tree = draw(Vec::new());
        let button = tree
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::Button && node.label() == Some("Play"))
            .expect("the playlist Play button")
            .0;
        draw(vec![egui::Event::AccessKitActionRequest(ActionRequest {
            target_tree: TreeId::ROOT,
            target_node: button,
            action: AccessibleAction::Click,
            data: None,
        })]);

        let request = app.engine.queued_play
            .as_ref()
            .expect("waiting for the local engine");
        assert_eq!(request.context_uri.as_deref(), Some(context));
        assert_eq!(request.offset_uri.as_deref(), Some(rows[0].as_str()));
        assert_eq!(request.position_ms, 0);
        assert!(
            request.uris.is_empty(),
            "retain the full Spotify playlist context"
        );
        let load = local_load(request, false);
        assert_eq!(load.offset_uri, Some(rows[0].clone()));
        assert_eq!(load.context_uri.as_deref(), Some(context));
        assert_eq!(app.engine.now_playing().unwrap().uri, rows[0]);
        assert_eq!(app.engine.now_playing().unwrap().position_ms, 0);
        app.engine.intent_track.as_mut().unwrap().at =
            Instant::now() - PLAYBACK_HOLD - Duration::from_secs(1);
        assert_eq!(app.engine.now_playing().unwrap().uri, rows[0]);
        assert_eq!(app.engine.current_track_uri().as_deref(), Some(rows[0].as_str()));

        // A ready notification may replay the pending request before the
        // engine connects. Keep the song chosen at the click even if the
        // playlist has refreshed in the meantime.
        app.engine.playlist_pages
            .get_mut("pl1")
            .unwrap()
            .items
            .items
            .swap(0, 7);
        app.engine.handle_playback(LocalPlayback::Ready {
            device_id: "demo-local".into(),
        });
        assert_eq!(
            app.engine.queued_play.as_ref().unwrap().offset_uri.as_deref(),
            Some(rows[0].as_str())
        );
        app.engine.handle_playback(LocalPlayback::Failed("test connection failure".into()));
        assert!(app.engine.requested_track_preview().is_none());
        app.engine.backend.shutdown();
    }

    /// A row in the Recent tab plays its own song. Each row plays a list
    /// holding only that song, but it hands over its place in the whole
    /// tab, which is past the end of that list for every row but the top.
    #[test]
    fn a_recent_row_plays_its_own_song() {
        use crate::api::models::{ArtistRef, PlayHistory, Track};
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, Role, TreeId};

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.engine.remote = None;
        app.engine.selected_device = None;
        app.engine.local.connected = false;
        app.engine.shuffle_wanted = false;
        assert!(matches!(app.engine.target(), Target::Local));
        app.engine.recents.items = ["newest", "middle", "oldest"]
            .iter()
            .enumerate()
            .map(|(index, id)| PlayHistory {
                track: Track {
                    id: Some((*id).into()),
                    uri: format!("spotify:track:{id}"),
                    name: format!("Recent {id}"),
                    artists: vec![ArtistRef {
                        name: "Recent Artist".into(),
                        ..Default::default()
                    }],
                    duration_ms: 200_000,
                    ..Default::default()
                },
                played_at: Some(format!("2026-09-01T1{}:00:00Z", 5 - index)),
                context: None,
            })
            .collect();
        app.engine.recents.loaded_once = true;
        app.engine.recents.complete = true;
        app.engine.rebuild_recents();
        app.engine.queue_tab = QueueTab::Recents;
        app.engine.show_queue_panel = true;

        let mut draw = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| crate::ui::queue::side_panel(&mut app, ui),
            );
            output.textures_delta.clear();
            app.apply_actions(&ctx);
            output.platform_output.accesskit_update.unwrap()
        };
        draw(Vec::new());
        let tree = draw(Vec::new());
        let row = tree
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == Role::Button
                    && node.label() == Some("Play Recent middle, Recent Artist")
            })
            .expect("the second row of the Recent tab")
            .0;
        draw(vec![egui::Event::AccessKitActionRequest(ActionRequest {
            target_tree: TreeId::ROOT,
            target_node: row,
            action: AccessibleAction::Click,
            data: None,
        })]);

        let request = app.engine.queued_play
            .as_ref()
            .expect("waiting for the local engine");
        assert_eq!(request.uris, ["spotify:track:middle"]);
        assert_eq!(
            request.offset_position,
            Some(0),
            "the song's place in the list it plays from"
        );
        assert_eq!(
            app.engine.intent_track.as_ref().map(|intent| intent.uri.as_str()),
            Some("spotify:track:middle"),
            "the chosen song is the one shown as starting"
        );

        // A row whose place holds its own song keeps that place, even when
        // the song is in the list earlier as well.
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Uris(
                    vec![
                        "spotify:track:middle".to_string(),
                        "spotify:track:newest".to_string(),
                        "spotify:track:middle".to_string(),
                    ]
                    .into(),
                ),
                uri: "spotify:track:middle".into(),
                index: 2,
            },
            &ctx,
        );
        assert_eq!(
            app.engine.queued_play
                .as_ref()
                .and_then(|request| request.offset_position),
            Some(2),
            "the second copy of a repeated song plays from its own place"
        );
        // The same row sends a valid one-song request to a Connect device,
        // with either shuffle setting, while its optimistic title stays visible.
        app.engine.selected_device = Some("speaker".into());
        for shuffle in [false, true] {
            app.engine.shuffle_wanted = shuffle;
            app.apply(
                Action::PlayFromRow {
                    context: RowContext::Uris(vec!["spotify:track:middle".into()].into()),
                    uri: "spotify:track:middle".into(),
                    index: 73,
                },
                &ctx,
            );
            let requests = app.engine.backend.take_remote_play_requests();
            assert_eq!(requests.len(), 1);
            let (device, play) = match &requests[0] {
                ApiRequest::Remote {
                    action: RemoteAction::Play,
                    device_id,
                    play: Some(play),
                    ..
                } if !shuffle => (device_id, play),
                ApiRequest::ShufflePlay { device_id, play } if shuffle => (device_id, play),
                other => panic!("unexpected playback request: {other:?}"),
            };
            assert_eq!(device.as_deref(), Some("speaker"));
            assert_eq!(play.uris, ["spotify:track:middle"]);
            assert_eq!(play.offset_position, Some(0));
            assert_eq!(
                app.engine.intent_track.as_ref().map(|intent| intent.uri.as_str()),
                Some("spotify:track:middle")
            );
        }
        app.engine.backend.shutdown();
    }

    fn draw_collection_actions(
        ctx: &egui::Context,
        app: &mut App,
        uri: &str,
        events: Vec<egui::Event>,
    ) {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                crate::ui::collection::actions_row(
                    app,
                    ui,
                    crate::ui::collection::Actions {
                        play_uri: Some(uri.to_owned()),
                        view: None,
                        saved: None,
                        saved_icons: (
                            crate::theme::Icon::CirclePlus,
                            crate::theme::Icon::CircleCheck,
                        ),
                        saved_tooltips: ("".into(), "".into()),
                        owned_playlist: None,
                        reload: None,
                        name: "Test",
                        save_radio: None,
                    },
                    None,
                );
            },
        );
        output.textures_delta.clear();
        app.apply_actions(ctx);
    }

    fn click_collection_action(
        ctx: &egui::Context,
        app: &mut App,
        uri: &str,
        position: egui::Pos2,
    ) {
        let click = vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        draw_collection_actions(ctx, app, uri, vec![]);
        draw_collection_actions(ctx, app, uri, click);
    }

    #[test]
    fn shuffle_selected_without_a_device_applies_when_collection_play_starts() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        let remote = app.engine.remote.take().expect("demo remote playback");
        app.engine.local_ready = false;
        app.engine.local_device_id = None;
        app.engine.local_playback = LocalPlayback::Unavailable;
        app.engine.local.connected = false;
        app.engine.selected_device = None;
        app.engine.shuffle_wanted = false;
        assert!(matches!(app.engine.target(), Target::Remote(None)));

        click_collection_action(
            &ctx,
            &mut app,
            "spotify:playlist:pl0",
            egui::pos2(87.0, 28.0),
        );
        assert!(app.engine.playing_context_shuffle());
        assert!(app.engine.remote.is_none(), "Shuffle must not start playback");
        assert!(app.engine.backend.take_remote_play_requests().is_empty());
        assert!(
            app.engine.backend.take_remote_shuffle_requests().is_empty(),
            "without an active device, Shuffle stays pending instead of calling Spotify"
        );
        assert!(
            app.engine.toasts.is_empty(),
            "selecting a pending mode shows no error"
        );

        let playing_context = remote
            .state
            .context
            .as_ref()
            .expect("demo playing context")
            .uri
            .clone();
        assert_ne!(playing_context, "spotify:playlist:pl0");
        app.engine.remote = Some(remote);
        click_collection_action(
            &ctx,
            &mut app,
            "spotify:playlist:pl0",
            egui::pos2(28.0, 28.0),
        );

        let requests = app.engine.backend.take_remote_play_requests();
        assert!(matches!(
            requests.as_slice(),
            [ApiRequest::ShufflePlay { device_id: Some(device), play }]
                if device == "remote1" && play.context_uri.as_deref() == Some("spotify:playlist:pl0")
        ));
        app.engine.backend.shutdown();
    }

    #[test]
    fn player_bar_uses_pending_shuffle_without_a_device() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, Toggled, TreeId};

        fn draw_player_bar(
            ctx: &egui::Context,
            app: &mut App,
            events: Vec<egui::Event>,
        ) -> egui::accesskit::TreeUpdate {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| crate::ui::player_bar::show(app, ui),
            );
            output.textures_delta.clear();
            app.apply_actions(ctx);
            output.platform_output.accesskit_update.unwrap()
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.engine.remote.take().expect("demo remote playback");
        app.engine.local_ready = false;
        app.engine.local_device_id = None;
        app.engine.local_playback = LocalPlayback::Unavailable;
        app.engine.local.connected = false;
        app.engine.selected_device = None;
        app.engine.shuffle_wanted = false;
        assert!(app.engine.now_playing().is_none());

        click_collection_action(
            &ctx,
            &mut app,
            "spotify:playlist:pl0",
            egui::pos2(87.0, 28.0),
        );
        assert!(app.engine.playing_context_shuffle());

        draw_player_bar(&ctx, &mut app, Vec::new());
        let enabled = draw_player_bar(&ctx, &mut app, Vec::new());
        let (shuffle_id, shuffle_node) = enabled
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Shuffle"))
            .expect("player-bar Shuffle button");
        assert_eq!(shuffle_node.toggled(), Some(Toggled::True));

        draw_player_bar(
            &ctx,
            &mut app,
            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                target_tree: TreeId::ROOT,
                target_node: *shuffle_id,
                action: AccessibleAction::Click,
                data: None,
            })],
        );
        assert!(!app.engine.playing_context_shuffle());

        let disabled = draw_player_bar(&ctx, &mut app, Vec::new());
        let shuffle_node = &disabled
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Shuffle"))
            .expect("player-bar Shuffle button")
            .1;
        assert_eq!(shuffle_node.toggled(), Some(Toggled::False));
        assert!(app.engine.backend.take_remote_shuffle_requests().is_empty());
        app.engine.backend.shutdown();
    }

    #[test]
    fn shuffle_targets_active_remote_playback_without_a_device_id() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.engine.local_ready = false;
        app.engine.selected_device = None;
        app.engine.shuffle_wanted = false;
        app.engine.remote
            .as_mut()
            .unwrap()
            .state
            .device
            .as_mut()
            .unwrap()
            .id = None;
        assert!(matches!(app.engine.target(), Target::Remote(None)));

        click_collection_action(
            &ctx,
            &mut app,
            "spotify:playlist:pl0",
            egui::pos2(87.0, 28.0),
        );
        assert!(matches!(
            app.engine.backend.take_remote_shuffle_requests().as_slice(),
            [ApiRequest::Remote {
                action: RemoteAction::Shuffle,
                device_id: None,
                flag: true,
                ..
            }]
        ));
        app.engine.backend.shutdown();
    }

    /// A remote device that refuses volume changes, such as a phone playing
    /// over Bluetooth, gets no volume request: Spotify would only answer
    /// with an error (#627).
    #[test]
    fn a_remote_device_without_volume_control_is_not_asked_to_change_it() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.engine.local_ready = false;
        app.engine.frame_now = None;
        let device = app.engine.remote.as_mut().unwrap().state.device.as_mut().unwrap();
        device.supports_volume = Some(false);
        device.volume_percent = Some(40);
        assert!(matches!(app.engine.target(), Target::Remote(_)));
        assert!(!app.engine.can_set_volume());
        assert!(!app.engine.now_playing().unwrap().can_set_volume);

        app.apply(Action::SetVolume(10), &ctx);
        app.apply(Action::VolumeBy(-5), &ctx);
        app.apply(Action::ToggleMute, &ctx);
        assert!(app.engine.pending_remote_volume.is_none());
        assert_eq!(app.engine.now_playing().unwrap().volume_percent, 40);

        let device = app.engine.remote.as_mut().unwrap().state.device.as_mut().unwrap();
        device.supports_volume = None;
        assert!(app.engine.can_set_volume());
        app.apply(Action::SetVolume(10), &ctx);
        assert!(matches!(app.engine.pending_remote_volume, Some((10, _))));
        app.engine.backend.shutdown();
    }

    /// The "Playing on" badge shows the playing device's own icon, so a
    /// phone is drawn as a phone rather than as a speaker.
    #[test]
    fn remote_playback_reports_the_device_kind_for_its_icon() {
        let mut app = headless_app();
        crate::demo::populate(&mut app);
        app.engine.local_ready = false;
        app.engine.frame_now = None;
        let device = app.engine.remote.as_mut().unwrap().state.device.as_mut().unwrap();
        device.name = "Galaxy S23".into();
        device.kind = "Smartphone".into();

        let now = app.engine.now_playing().unwrap();
        assert!(!now.local);
        assert_eq!(now.device_kind.as_deref(), Some("Smartphone"));
        assert!(matches!(
            crate::ui::devices::device_icon(now.device_kind.as_deref().unwrap()),
            crate::theme::Icon::Smartphone
        ));
        app.engine.backend.shutdown();
    }

    #[test]
    fn shuffle_toggle_on_another_collection_keeps_playing_context_until_play() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.engine.shuffle_wanted = true;
        let playing_context = app.engine.playing_context_uri()
            .expect("demo is playing a collection");
        let other_collection = "spotify:playlist:pl0";
        assert_ne!(playing_context, other_collection);
        app.engine.open(Page::Playlist("pl0".into()));

        click_collection_action(&ctx, &mut app, other_collection, egui::pos2(87.0, 28.0));
        assert!(!app.engine.playing_context_shuffle());
        assert_eq!(
            app.engine.playing_context_uri().as_deref(),
            Some(playing_context.as_str())
        );
        assert!(
            app.engine.backend.take_remote_play_requests().is_empty(),
            "changing the global mode must not start the viewed collection"
        );

        click_collection_action(&ctx, &mut app, other_collection, egui::pos2(28.0, 28.0));
        let requests = app.engine.backend.take_remote_play_requests();
        assert!(matches!(
            requests.as_slice(),
            [ApiRequest::Remote {
                action: RemoteAction::Play,
                device_id: Some(device),
                play: Some(play),
                ..
            }] if device == "remote1" && play.context_uri.as_deref() == Some(other_collection)
        ));
        app.engine.backend.shutdown();
    }

    #[test]
    fn collection_play_starts_at_the_first_available_row_in_the_shown_view() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, Role, TreeId};
        for liked in [false, true] {
            for filter in [None, Some("First"), Some("Unavailable"), Some("Missing")] {
                let filtered = filter.is_some();
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                let mut app = headless_app();
                app.attach(&ctx);
                crate::demo::populate(&mut app);
                app.engine.remote = None;
                app.engine.selected_device = None;
                app.engine.local.connected = false;
                app.engine.shuffle_wanted = false;
                let track = |id: &str, name: &str, available, local| Track {
                    id: Some(id.into()),
                    uri: if local {
                        "spotify:local:Artist:Album:Song:180".into()
                    } else {
                        format!("spotify:track:{id}")
                    },
                    name: name.into(),
                    is_playable: Some(available),
                    is_local: local,
                    duration_ms: 180_000,
                    ..Default::default()
                };
                let tracks = [
                    track("other", "03 Other", true, false),
                    track("unavailable", "00 Unavailable", false, false),
                    track("local", "01 Local", true, true),
                    track("first", "02 First", true, false),
                    track("first", "02 First", true, false),
                ];
                let page = if liked {
                    Page::LikedSongs
                } else {
                    Page::Playlist("pl1".into())
                };
                if liked {
                    app.engine.library.liked.items = tracks
                        .iter()
                        .map(|track| crate::api::models::SavedTrack {
                            track: track.clone(),
                            added_at: None,
                        })
                        .collect();
                    app.engine.library.liked.total = Some(5);
                    app.engine.library.liked.next_offset = None;
                    app.engine.library.liked.revision += 1;
                    if let Some(filter) = filter {
                        ctx.data_mut(|data| {
                            data.insert_temp(egui::Id::new("liked-filter"), filter.to_string())
                        });
                    }
                } else {
                    let list = app.engine.playlist_pages.get_mut("pl1").unwrap();
                    list.items.items = tracks
                        .iter()
                        .map(|track| PlaylistItem {
                            // Spotify can mark only the enclosing playlist row local.
                            is_local: track.is_local,
                            item: Some(PlayableItem::Track(Track {
                                is_local: false,
                                ..track.clone()
                            })),
                            ..Default::default()
                        })
                        .collect();
                    list.items.total = Some(5);
                    list.items.next_offset = None;
                    list.items.revision += 1;
                    if let Some(filter) = filter {
                        list.filter = filter.into();
                    }
                }
                if !filtered {
                    app.engine.table_sorts.insert(
                        page.clone(),
                        TableSort {
                            column: SortColumn::Title,
                            ascending: true,
                        },
                    );
                }
                app.engine.open(page);
                let mut draw = |events| {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(1280.0, 800.0),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            if liked {
                                crate::ui::collection::liked(&mut app, ui);
                            } else {
                                crate::ui::collection::playlist(&mut app, ui, "pl1");
                            }
                        },
                    );
                    output.textures_delta.clear();
                    app.apply_actions(&ctx);
                    output.platform_output.accesskit_update.unwrap()
                };
                draw(vec![]);
                let tree = draw(vec![]);
                let button = tree
                    .nodes
                    .iter()
                    .find(|(_, node)| node.role() == Role::Button && node.label() == Some("Play"))
                    .expect("collection Play")
                    .0;
                draw(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    target_tree: TreeId::ROOT,
                    target_node: button,
                    action: AccessibleAction::Click,
                    data: None,
                })]);
                if matches!(filter, Some("Unavailable" | "Missing")) {
                    let play = tree.nodes.iter().find(|(id, _)| *id == button).unwrap();
                    assert!(
                        play.1.is_disabled(),
                        "a view without playable songs disables Play"
                    );
                    assert!(
                        app.engine.queued_play.is_none(),
                        "do not start the unfiltered context when no shown song can play"
                    );
                    app.engine.backend.shutdown();
                    continue;
                }
                let request = app.engine.queued_play
                    .as_ref()
                    .expect("waiting for local playback");
                if filtered {
                    assert_eq!(
                        request.uris,
                        vec!["spotify:track:first", "spotify:track:first"],
                        "a filtered view keeps only its shown songs and preserves duplicates"
                    );
                    assert_eq!(request.offset_position, Some(0));
                } else {
                    assert_eq!(
                        request.uris,
                        vec![
                            "spotify:track:first",
                            "spotify:track:first",
                            "spotify:track:other"
                        ]
                    );
                    assert_eq!(
                        request.offset_position,
                        Some(0),
                        "playback starts at the first playable row"
                    );
                }
                assert_eq!(app.engine.now_playing().unwrap().uri, "spotify:track:first");
                let mut settled = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280.0, 800.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        if liked {
                            crate::ui::collection::liked(&mut app, ui);
                        } else {
                            crate::ui::collection::playlist(&mut app, ui, "pl1");
                        }
                    },
                );
                settled.textures_delta.clear();
                app.apply_actions(&ctx);
                let tree = settled.platform_output.accesskit_update.unwrap();
                let second = tree
                    .nodes
                    .iter()
                    .filter(|(_, node)| {
                        node.role() == Role::Button
                            && node
                                .label()
                                .is_some_and(|label| label.starts_with("Play 02 First,"))
                    })
                    .max_by(|a, b| {
                        a.1.bounds()
                            .unwrap()
                            .y0
                            .total_cmp(&b.1.bounds().unwrap().y0)
                    })
                    .expect("second duplicate row")
                    .0;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280.0, 800.0),
                        )),
                        events: vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            target_tree: TreeId::ROOT,
                            target_node: second,
                            action: AccessibleAction::Click,
                            data: None,
                        })],
                        ..Default::default()
                    },
                    |ui| {
                        if liked {
                            crate::ui::collection::liked(&mut app, ui);
                        } else {
                            crate::ui::collection::playlist(&mut app, ui, "pl1");
                        }
                    },
                );
                output.textures_delta.clear();
                assert!(
                    app.engine.actions
                        .iter()
                        .any(|action| matches!(action, Action::PlayFromRow { index: 1, .. })),
                    "second occurrence action: {:?}",
                    app.engine.actions
                );
                app.apply_actions(&ctx);
                assert_eq!(
                    app.engine.queued_play.as_ref().unwrap().offset_position,
                    Some(1),
                    "selecting the second duplicate keeps its occurrence in the playable list"
                );
                app.engine.backend.shutdown();
            }
        }
    }

    #[test]
    fn sorted_view_play_shows_an_uncached_song_while_local_playback_connects() {
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        let ctx = egui::Context::default();
        let first = Track {
            id: Some("first".into()),
            uri: "spotify:track:first".into(),
            name: "First in the sorted view".into(),
            duration_ms: 180_000,
            ..Default::default()
        };
        app.engine.playlist_pages.insert(
            "mix".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:other"),
                        PlaylistItem {
                            item: Some(PlayableItem::Track(first.clone())),
                            ..Default::default()
                        },
                    ],
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(!app.engine.track_cache.contains_key("first"));
        app.apply(
            Action::PlayFromRow {
                context: RowContext::View {
                    context_uri: "spotify:playlist:mix".into(),
                    uris: vec![first.uri.clone(), "spotify:track:other".into()].into(),
                    editable_playlist: None,
                },
                uri: String::new(),
                index: 0,
            },
            &ctx,
        );
        let now = app.engine.now_playing()
            .expect("the requested song appears before the engine connects");
        assert_eq!(now.uri, first.uri);
        assert_eq!(now.title, first.name);
        assert_eq!(now.position_ms, 0);
        assert!(now.loading);
        assert_eq!(
            app.engine.queued_play.as_ref().unwrap().uris,
            vec![first.uri.clone(), "spotify:track:other".into()]
        );
        assert_eq!(
            app.engine.playlist_pages["mix"].items.items[0]
                .playable()
                .unwrap()
                .uri(),
            "spotify:track:other",
            "previewing must not change the playlist order"
        );
        app.engine.intent_track.as_mut().unwrap().at =
            Instant::now() - PLAYBACK_HOLD - Duration::from_secs(1);
        assert_eq!(
            app.engine.now_playing().unwrap().uri,
            first.uri,
            "the preview stays while the play request is pending"
        );
    }

    #[test]
    fn each_repeat_of_a_local_short_song_must_earn_its_own_history_entry() {
        let mut app = headless_app();
        app.engine.remote = None;
        app.engine.selected_device = None;
        app.engine.plays = crate::history::History::default();
        let state_dir =
            std::env::temp_dir().join(format!("spotifast-repeat-history-{}", std::process::id()));
        app.engine.dirs.state = state_dir.clone();
        for sequence in [1, 2] {
            app.handle_local(LocalState {
                connected: true,
                playback: Playback::Playing,
                track_sequence: sequence,
                track: Some(crate::player::LocalTrack {
                    uri: "spotify:track:short".into(),
                    title: "Short interlude".into(),
                    duration_ms: 40_000,
                    ..Default::default()
                }),
                ..Default::default()
            });
            app.engine.note_listening();
            assert_eq!(app.engine.plays.plays().len(), sequence as usize - 1);
            let listening = app.engine.listening.as_mut().unwrap();
            assert!(
                !listening.recorded,
                "the new play must be counted separately"
            );
            listening.playing_since = Some(Instant::now() - Duration::from_secs(21));
            app.engine.note_listening();
            assert_eq!(app.engine.plays.plays().len(), sequence as usize);
            assert_eq!(app.engine.recents_view.len(), sequence as usize);

            // Seeking or pausing the same play cannot count it again.
            let mut paused = app.engine.local.clone();
            paused.playback = Playback::Paused;
            paused.seek_sequence += 1;
            app.handle_local(paused);
            app.engine.note_listening();
            let mut resumed = app.engine.local.clone();
            resumed.playback = Playback::Playing;
            app.handle_local(resumed);
            app.engine.note_listening();
            assert_eq!(app.engine.plays.plays().len(), sequence as usize);
        }
        app.engine.backend.shutdown();
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn playlist_start_preview_waits_for_playback_before_consuming_queue_or_history() {
        let mut app = headless_app();
        crate::demo::populate(&mut app);
        app.engine.remote = None;
        app.engine.selected_device = None;
        app.engine.shuffle_wanted = false;
        let rows = app.engine.context_track_uris("spotify:playlist:pl1").unwrap();
        let old = crate::player::LocalTrack {
            uri: rows[7].clone(),
            duration_ms: 200_000,
            ..Default::default()
        };
        app.engine.local = LocalState {
            connected: true,
            track: Some(old),
            playback: Playback::Playing,
            position_ms: 23_000,
            ..Default::default()
        };
        app.engine.last_now_playing_uri = Some(rows[7].clone());
        app.engine.manual_queue = vec![rows[0].clone()];
        app.engine.play_request(PlayRequest::context("spotify:playlist:pl1"), false);
        assert!(
            app.engine.queued_play.is_none(),
            "the connected engine received it"
        );
        assert_eq!(app.engine.now_playing().unwrap().uri, rows[0]);
        assert!(app.engine.now_playing().unwrap().loading);

        // A progress report for the old song cannot undo the user's start.
        let mut stale = app.engine.local.clone();
        stale.position_ms += 1;
        app.handle_local(stale);
        app.refresh_frame_now();
        assert_eq!(app.engine.now_playing().unwrap().uri, rows[0]);
        app.on_now_playing_changed();
        assert_eq!(app.engine.manual_queue, vec![rows[0].clone()]);
        app.engine.note_listening();
        assert_eq!(app.engine.listening.as_ref().unwrap().uri, rows[7]);

        let mut started = app.engine.local.clone();
        started.track = Some(crate::player::LocalTrack {
            uri: rows[0].clone(),
            duration_ms: 200_000,
            ..Default::default()
        });
        started.position_ms = 0;
        app.handle_local(started);
        app.refresh_frame_now();
        assert_eq!(app.engine.now_playing().unwrap().uri, rows[0]);
        assert!(!app.engine.now_playing().unwrap().loading);
        assert!(app.engine.intent_track.is_none());
        assert_eq!(
            app.engine.manual_queue,
            vec![rows[0].clone()],
            "starting the playlist preserves the separately queued copy"
        );
        app.engine.note_listening();
        assert_eq!(app.engine.listening.as_ref().unwrap().uri, rows[0]);
        app.engine.backend.shutdown();
    }

    /// Every skip and end-of-track advance loads the next track while the
    /// previous one still shows as playing. The engine's loading report must
    /// turn the transport's spinner on without disturbing that playing face,
    /// and the track that starts must turn it off again.
    #[test]
    fn a_load_reported_mid_song_shows_the_button_spinner() {
        let mut app = headless_app();
        app.engine.remote = None;
        app.engine.selected_device = None;
        app.engine.local = LocalState {
            connected: true,
            track: Some(crate::player::LocalTrack {
                uri: "spotify:track:a".into(),
                title: "A".into(),
                duration_ms: 100_000,
                ..Default::default()
            }),
            playback: Playback::Playing,
            position_ms: 30_000,
            ..Default::default()
        };

        let mut loading = app.engine.local.clone();
        loading.loading = true;
        app.handle_local(loading);
        app.refresh_frame_now();
        let now = app.engine.now_playing().unwrap();
        assert!(now.playing, "the transport keeps its playing face");
        assert!(now.loading, "the disc follows the engine's load");

        let mut started = app.engine.local.clone();
        started.loading = false;
        started.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:b".into(),
            title: "B".into(),
            duration_ms: 100_000,
            ..Default::default()
        });
        started.track_sequence += 1;
        app.handle_local(started);
        app.refresh_frame_now();
        assert!(!app.engine.now_playing().unwrap().loading);
        app.engine.backend.shutdown();
    }

    #[test]
    fn playlist_play_without_its_prefix_requests_position_zero() {
        for base_offset in [None, Some(0), Some(6_900)] {
            let mut app = headless_app();
            if let Some(base_offset) = base_offset {
                app.engine.playlist_pages.insert(
                    "large".into(),
                    PlaylistPage {
                        items: PagedList {
                            base_offset,
                            items: if base_offset == 0 {
                                Vec::new()
                            } else {
                                vec![cached_playlist_row("spotify:track:middle")]
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                );
            }
            app.engine.play_request(PlayRequest::context("spotify:playlist:large"), false);
            let request = app.engine.queued_play.as_ref().unwrap();
            assert_eq!(request.offset_uri, None, "base offset {base_offset:?}");
            assert_eq!(request.offset_position, Some(0));
            let load = local_load(request, false);
            assert_eq!(load.context_uri.as_deref(), Some("spotify:playlist:large"));
            assert_eq!(load.offset_index, Some(0));
            assert!(load.uris.is_empty());
        }
    }

    #[test]
    fn playlist_play_skips_missing_local_and_unavailable_prefix_rows() {
        let mut app = headless_app();
        let mut local = cached_playlist_row("spotify:local:artist:album:track");
        local.is_local = true;
        let mut unavailable = cached_playlist_row("spotify:track:unavailable");
        let Some(PlayableItem::Track(track)) = unavailable.item.as_mut() else {
            panic!("a track fixture");
        };
        track.is_playable = Some(false);
        app.engine.playlist_pages.insert(
            "playlist".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        Default::default(),
                        local,
                        unavailable,
                        cached_playlist_row("spotify:track:first"),
                        cached_playlist_row("spotify:track:second"),
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.engine.play_request(PlayRequest::context("spotify:playlist:playlist"), false);
        assert_eq!(
            app.engine.queued_play.as_ref().unwrap().offset_uri.as_deref(),
            Some("spotify:track:first")
        );
    }

    #[test]
    fn playlist_start_keeps_explicit_rows_and_shuffle_choices() {
        let mut app = headless_app();
        app.engine.playlist_pages.insert(
            "playlist".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let context = "spotify:playlist:playlist";
        for shuffle in [false, true] {
            app.engine.shuffle_wanted = shuffle;
            for request in [
                PlayRequest::context(context).starting_at_uri("spotify:track:chosen"),
                PlayRequest::context(context).starting_at_index(7),
                PlayRequest::tracks(vec!["spotify:track:sorted-first".into()]).starting_at_index(0),
            ] {
                app.engine.play_request(request.clone(), false);
                let queued = app.engine.queued_play.as_ref().unwrap();
                assert_eq!(queued.offset_uri, request.offset_uri);
                assert_eq!(queued.offset_position, request.offset_position);
                assert_eq!(queued.uris, request.uris);
            }
        }
        // With no loaded prefix, local shuffle must still choose its own
        // random start, rather than receiving the unshuffled position zero.
        app.engine.playlist_pages.clear();
        app.engine.play_request(PlayRequest::context(context), false);
        let request = app.engine.queued_play.as_ref().unwrap();
        assert_eq!(request.offset_uri, None);
        assert_eq!(request.offset_position, None);
    }

    #[test]
    fn failed_proxy_migration_preserves_the_original_until_storage_is_confirmed() {
        let mut app = test_app("proxy-migration-settings");
        app.engine.offline = false;
        std::fs::create_dir_all(&app.engine.dirs.config).unwrap();
        let original = r#"{"proxy_mode":"http","proxy_host":"127.0.0.1","proxy_port":"8080","proxy_password":"dummy-legacy-secret"}"#;
        std::fs::write(app.engine.dirs.settings_file(), original).unwrap();
        app.engine.settings = Settings::load(&app.engine.dirs.settings_file());
        app.applied_proxy_preferences = app.engine.settings.proxy_preferences();
        app.engine.settings.theme = crate::settings::ThemeChoice::Light;
        app.engine.settings.proxy_host = "other.example".into();
        app.save_settings();
        app.engine.flush_files();
        assert_eq!(
            std::fs::read_to_string(app.engine.dirs.settings_file()).unwrap(),
            original
        );
        assert!(
            !app.engine.settings_dirty,
            "do not retry every frame while storage is pending"
        );
        app.handle_proxy_password_stored();
        app.engine.flush_files();
        let saved = Settings::load(&app.engine.dirs.settings_file());
        assert_eq!(saved.theme, crate::settings::ThemeChoice::Light);
        assert_eq!(
            saved.proxy_host, "127.0.0.1",
            "unapplied draft is not saved"
        );
        assert!(saved.proxy_password.is_empty());
        assert!(!saved.proxy_password_legacy);
        assert!(
            !std::fs::read_to_string(app.engine.dirs.settings_file())
                .unwrap()
                .contains("dummy-legacy-secret")
        );
    }

    #[test]
    fn a_separate_legacy_password_cannot_be_rebound_by_saving_a_new_address() {
        let seed = test_app("proxy-migration-separate");
        let dirs = seed.engine.dirs.clone();
        drop(seed);
        std::fs::create_dir_all(&dirs.state).unwrap();
        let settings = Settings {
            proxy_mode: crate::settings::ProxyMode::Http,
            proxy_host: "127.0.0.1".into(),
            proxy_port: "8080".into(),
            ..Default::default()
        };
        settings.save(&dirs.settings_file());
        std::fs::write(dirs.proxy_secret_file(), "dummy-legacy-secret").unwrap();
        let original = std::fs::read(dirs.settings_file()).unwrap();
        let mut app = App::new(
            &Waker::default(),
            dirs,
            settings,
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        assert!(app.engine.settings.proxy_password_legacy);
        app.engine.settings.proxy_host = "other.example".into();
        app.request_proxy(false);
        app.handle_proxy_applied(
            app.engine.proxy_request,
            app.engine.settings.proxy_config().unwrap(),
            Ok(false),
        );
        app.engine.flush_files();
        assert_eq!(std::fs::read(app.engine.dirs.settings_file()).unwrap(), original);
        // An unlocked native store can finish the launch-time migration while
        // this test runs, in which case deleting the plaintext file is right.
        // If migration is still pending, changing the address must leave the
        // only surviving copy untouched.
        match std::fs::read_to_string(app.engine.dirs.proxy_secret_file()) {
            Ok(password) => assert_eq!(password, "dummy-legacy-secret"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("could not read the legacy proxy password: {error}"),
        }
    }

    #[test]
    fn late_proxy_password_restore_respects_form_edits_and_applied_settings() {
        for edited in [false, true] {
            let mut app = test_app(if edited {
                "proxy-restored-edit"
            } else {
                "proxy-restored-initial"
            });
            app.engine.settings.proxy_mode = crate::settings::ProxyMode::Http;
            app.engine.settings.proxy_host = "127.0.0.1".into();
            app.engine.settings.proxy_port = "8080".into();
            let mut stored = app.engine.settings.clone();
            stored.proxy_password = "dummy-saved-password".into();
            app.engine.proxy_form_edited = edited;
            app.engine.handle_proxy_restored(
                stored.proxy_config().unwrap(),
                stored.proxy_password_record().unwrap(),
            );
            assert_eq!(app.engine.settings.proxy_password.is_empty(), edited);
            app.engine.settings.proxy_mode = crate::settings::ProxyMode::Off;
            app.request_proxy(false);
            app.handle_proxy_applied(
                app.engine.proxy_request,
                crate::settings::ProxyConfig::Off,
                Ok(false),
            );
            app.engine.settings.proxy_password.clear();
            app.engine.handle_proxy_restored(
                stored.proxy_config().unwrap(),
                stored.proxy_password_record().unwrap(),
            );
            assert_eq!(app.engine.applied_proxy, crate::settings::ProxyConfig::Off);
            assert!(app.engine.settings.proxy_password.is_empty());
        }
    }

    #[test]
    fn a_rejected_proxy_change_does_not_announce_success() {
        let mut app = test_app("proxy-rejected");
        app.engine.backend.set_offline(true);
        let original = app.engine.applied_proxy.clone();
        let failed = crate::settings::ProxyConfig::Invalid("Proxy port must be a number".into());
        app.request_proxy(false);
        app.handle_proxy_applied(
            app.engine.proxy_request,
            failed,
            Err("Unable to build the configured client".into()),
        );
        assert_eq!(app.engine.applied_proxy, original);
        assert!(
            app.engine.toasts
                .iter()
                .any(|toast| toast.message.contains("could not be applied"))
        );
        app.engine.backend.shutdown();
    }

    #[test]
    fn saving_other_settings_keeps_proxy_drafts_out_of_the_next_launch() {
        let mut app = test_app("proxy-persisted-draft");
        app.engine.backend.set_offline(true);
        app.engine.settings.proxy_mode = crate::settings::ProxyMode::Http;
        app.engine.settings.proxy_host = "127.0.0.1".into();
        app.engine.settings.proxy_port = "8080".into();
        app.engine.settings.proxy_password = "dummy-private-password".into();
        app.engine.settings.theme = ThemeChoice::Light;
        app.save_settings();
        app.engine.flush_files();
        let saved = Settings::load(&app.engine.dirs.settings_file());
        assert_eq!(saved.proxy_mode, crate::settings::ProxyMode::System);
        assert_eq!(saved.theme, ThemeChoice::Light);
        assert!(
            !std::fs::read_to_string(app.engine.dirs.settings_file())
                .unwrap()
                .contains("dummy-private-password")
        );
        assert!(
            !app.engine.dirs.proxy_secret_file().exists(),
            "never write a plaintext password beside settings"
        );
        app.request_proxy(false);
        let request = app.engine.proxy_request;
        let accepted = app.engine.settings.proxy_config().unwrap();
        // An acknowledgement for port 8080 arrives after another form edit.
        app.engine.settings.proxy_port = "8090".into();
        app.handle_proxy_applied(request, accepted, Ok(false));
        assert_eq!(
            app.engine.settings.proxy_port, "8090",
            "keep the draft being edited"
        );
        app.engine.flush_files();
        assert_eq!(Settings::load(&app.engine.dirs.settings_file()).proxy_port, "8080");
        app.engine.backend.shutdown();
    }

    #[test]
    fn an_old_proxy_acknowledgement_cannot_replace_a_newer_applied_policy() {
        let mut app = test_app("proxy-stale-ack");
        app.engine.backend.set_offline(true);
        app.engine.settings.proxy_mode = crate::settings::ProxyMode::Http;
        app.engine.settings.proxy_host = "127.0.0.1".into();
        app.engine.settings.proxy_port = "8080".into();
        app.request_proxy(false);
        let first = app.engine.proxy_request;
        let old = app.engine.settings.proxy_config().unwrap();
        app.engine.settings.proxy_port = "8090".into();
        app.request_proxy(false);
        let newest = app.engine.settings.proxy_config().unwrap();
        app.handle_proxy_applied(app.engine.proxy_request, newest.clone(), Ok(false));
        app.handle_proxy_applied(first, old, Ok(false));
        assert_eq!(app.engine.applied_proxy, newest);
        app.engine.flush_files();
        assert_eq!(Settings::load(&app.engine.dirs.settings_file()).proxy_port, "8090");
        app.engine.backend.shutdown();
    }

    #[test]
    fn manual_proxy_edits_remain_drafts_until_apply() {
        let mut app = test_app("proxy-draft");
        app.engine.backend.set_offline(true);
        let original = app.engine.applied_proxy.clone();
        app.engine.settings.proxy_mode = crate::settings::ProxyMode::Http;
        app.engine.settings.proxy_host = "127.0.0.1".into();
        app.engine.settings.proxy_port = "8080".into();

        app.engine.actions.push(Action::RestartEngine);
        app.apply_actions(&egui::Context::default());
        assert_eq!(app.engine.applied_proxy, original);

        app.engine.actions.push(Action::ApplyProxy);
        app.apply_actions(&egui::Context::default());
        assert_eq!(
            app.engine.applied_proxy, original,
            "wait for the transport to build"
        );
        app.handle_proxy_applied(
            app.engine.proxy_request,
            app.engine.settings.proxy_config().unwrap(),
            Ok(true),
        );
        assert!(matches!(
            app.engine.applied_proxy,
            crate::settings::ProxyConfig::Http(_)
        ));
    }

    /// Shuffle picks a random loaded track or Web API offset. Local librespot
    /// playback chooses its own starting track.
    #[test]
    fn a_shuffled_play_does_not_start_at_track_one() {
        use crate::api::models::{
            Album, PlayableItem, Playlist, PlaylistItem, SavedAlbum, Track, TrackCount,
        };
        let track = |uri: &str| {
            Some(PlayableItem::Track(Track {
                uri: uri.into(),
                ..Default::default()
            }))
        };
        let mut app = headless_app();
        app.engine.library.playlists = Loadable::Loaded(vec![
            Playlist {
                uri: "spotify:playlist:open".into(),
                tracks: Some(TrackCount { total: 3 }),
                ..Default::default()
            },
            Playlist {
                uri: "spotify:playlist:unopened".into(),
                tracks: Some(TrackCount { total: 57 }),
                ..Default::default()
            },
        ]);
        app.engine.library.albums.items = vec![SavedAlbum {
            album: Album {
                uri: "spotify:album:saved".into(),
                total_tracks: Some(12),
                ..Default::default()
            },
            ..Default::default()
        }];
        app.engine.library.liked.total = Some(9);
        app.engine.playlist_pages.insert(
            "open".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        PlaylistItem {
                            item: track("spotify:track:one"),
                            ..Default::default()
                        },
                        PlaylistItem {
                            item: track("spotify:track:two"),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        // Playing on a phone: the Web API needs the offset.
        app.engine.selected_device = Some("phone".into());
        assert!(matches!(app.engine.target(), Target::Remote(Some(_))));

        // The playlist on screen: the start is one of its own rows.
        let (uri, position) = app.engine.shuffle_start("spotify:playlist:open");
        assert!(
            matches!(
                uri.as_deref(),
                Some("spotify:track:one" | "spotify:track:two")
            ),
            "the start comes from the rows, got {uri:?}"
        );
        assert_eq!(position, None);

        // Begun from the sidebar or a menu, with no rows loaded: a
        // position inside the length the library reported.
        for (context, len) in [
            ("spotify:playlist:unopened", 57),
            ("spotify:album:saved", 12),
            ("spotify:user:someone:collection", 9),
        ] {
            for _ in 0..50 {
                let (uri, position) = app.engine.shuffle_start(context);
                assert_eq!(uri, None, "{context} has no rows to name");
                let position = position.unwrap_or_else(|| panic!("{context} got no offset"));
                assert!(
                    position < len,
                    "{context} offset {position} is outside {len}"
                );
            }
        }
        // Over 50 draws a 57-song playlist should not have sat still on
        // one song, let alone on the first.
        let drawn: std::collections::HashSet<Option<u32>> = (0..50)
            .map(|_| app.engine.shuffle_start("spotify:playlist:unopened").1)
            .collect();
        assert!(drawn.len() > 1, "the starting position never moved");

        // Nothing saved, nothing loaded: no offset to give.
        assert_eq!(app.engine.shuffle_start("spotify:playlist:unknown"), (None, None));

        // Local playback: librespot picks the starting track itself.
        app.engine.selected_device = None;
        assert!(matches!(app.engine.target(), Target::Local));
        assert_eq!(
            app.engine.shuffle_start("spotify:playlist:unopened"),
            (None, None),
            "librespot is left to draw its own"
        );
    }

    /// A failed refresh retains visible cards and stale responses cannot replace them.
    #[test]
    fn made_for_you_keeps_the_last_shelf_on_error_and_ignores_old_answers() {
        let mut app = headless_app();
        app.engine.home.made_for_you_generation = 4;
        let playlist = crate::api::models::Playlist {
            id: "1234567890123456789012".into(),
            name: "Your current mix".into(),
            ..Default::default()
        };
        app.handle_api(ApiResponse::MadeForYou {
            personalized: true,
            generation: 4,
            result: Ok(vec![playlist.clone()]),
        });
        assert_eq!(
            app.engine.home.made_for_you.get().unwrap(),
            &vec![playlist.clone()]
        );
        app.handle_api(ApiResponse::MadeForYou {
            personalized: true,
            generation: 4,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(app.engine.home.made_for_you.get().unwrap(), &vec![playlist]);
        app.handle_api(ApiResponse::MadeForYou {
            personalized: true,
            generation: 3,
            result: Ok(vec![]),
        });
        assert_eq!(app.engine.home.made_for_you.get().unwrap().len(), 1);
    }

    /// Playback readiness upgrades fallback cards without blanking or double-counting the shelf.
    #[test]
    fn playback_ready_replaces_fallback_cards_and_ignores_late_fallbacks() {
        for fallback_finished in [false, true] {
            let mut app = headless_app();
            app.engine.local_ready = false;
            app.engine.load_home(false);
            let old = app.engine.home.made_for_you_generation;
            let fallback = vec![Playlist {
                id: "fallback".into(),
                ..Default::default()
            }];
            if fallback_finished {
                app.handle_api(ApiResponse::MadeForYou {
                    generation: old,
                    personalized: false,
                    result: Ok(fallback.clone()),
                });
            } else {
                app.engine.restore_home(HomeSnapshot {
                    made_for_you: Some(fallback.clone()),
                    made_for_you_personalized: true,
                    ..Default::default()
                });
            }
            app.engine.handle_playback(LocalPlayback::Ready {
                device_id: "local".into(),
            });
            let current = app.engine.home.made_for_you_generation;
            assert!(current > old);
            assert_eq!(app.engine.home.awaiting, 4);
            assert_eq!(app.engine.home.made_for_you.get(), Some(&fallback));
            app.engine.handle_playback(LocalPlayback::Ready {
                device_id: "local".into(),
            });
            assert_eq!(app.engine.home.made_for_you_generation, current);
            app.handle_api(ApiResponse::MadeForYou {
                generation: current,
                personalized: true,
                result: Ok(vec![Playlist {
                    id: "personalized".into(),
                    ..Default::default()
                }]),
            });
            app.handle_api(ApiResponse::MadeForYou {
                generation: old,
                personalized: false,
                result: Ok(fallback),
            });
            assert_eq!(app.engine.home.made_for_you.get().unwrap()[0].id, "personalized");
            assert!(app.engine.home.made_for_you_personalized);
            assert_eq!(app.engine.home.awaiting, 3);
        }
    }

    /// A queued shelf response from before an account reset cannot reuse its request serial.
    #[test]
    fn made_for_you_request_serial_survives_account_reset() {
        let mut app = headless_app();
        app.engine.load_home(false);
        let old = app.engine.home.made_for_you_generation;
        app.engine.reset_data();
        app.engine.load_home(false);
        assert!(app.engine.home.made_for_you_generation > old);
        app.handle_api(ApiResponse::MadeForYou {
            generation: old,
            personalized: true,
            result: Ok(Vec::new()),
        });
        assert!(app.engine.home.made_for_you.is_loading());
        assert_eq!(app.engine.home.awaiting, 4);
    }

    /// Personalized cache order and origin survive a restart; late disk data cannot replace a live shelf.
    #[test]
    fn personalized_home_cache_preserves_order_and_live_answers() {
        let mut app = listener_app();
        app.engine.load_home(false);
        let cards = vec![
            Playlist {
                id: "second".into(),
                ..Default::default()
            },
            Playlist {
                id: "first".into(),
                ..Default::default()
            },
        ];
        app.handle_api(ApiResponse::MadeForYou {
            generation: app.engine.home.made_for_you_generation,
            personalized: true,
            result: Ok(cards.clone()),
        });
        let saved = app.engine.backend.take_home_cache_stores().pop().unwrap().data;
        assert!(saved.made_for_you_personalized);
        assert_eq!(saved.made_for_you, Some(cards.clone()));
        let mut next = listener_app();
        next.engine.restore_home(saved);
        assert_eq!(next.engine.home.made_for_you.get(), Some(&cards));
        assert!(next.engine.home.made_for_you_personalized);
        next.engine.restore_home(cached_home());
        assert_eq!(next.engine.home.made_for_you.get(), Some(&cards));
    }

    #[test]
    fn a_free_account_is_told_once_that_it_cannot_play() {
        let me = |product: &str| {
            ApiResponse::Me(Ok(crate::api::models::User {
                id: "someone".into(),
                product: Some(product.into()),
                ..Default::default()
            }))
        };
        let mut app = headless_app();
        app.handle_api(me("free"));
        assert!(matches!(app.engine.dialog, Some(Dialog::PremiumNeeded)));
        app.engine.dialog = None;
        app.handle_api(me("free"));
        assert!(app.engine.dialog.is_none(), "the notice is shown once");

        let mut app = headless_app();
        app.handle_api(me("premium"));
        assert!(app.engine.dialog.is_none());
    }

    /// One song plays as a context of its own, so librespot's autoplay
    /// follows it; a list stays a list.
    #[test]
    fn one_song_is_loaded_as_a_context() {
        let one = local_load(&PlayRequest::tracks(vec!["spotify:track:a".into()]), false);
        assert_eq!(one.context_uri.as_deref(), Some("spotify:track:a"));
        assert!(one.uris.is_empty() && !one.autoplay);
        let two = local_load(
            &PlayRequest::tracks(vec!["spotify:track:a".into(), "spotify:track:b".into()])
                .starting_at_index(1),
            true,
        );
        assert_eq!(two.context_uri, None);
        assert_eq!(two.uris.len(), 2);
        assert_eq!(two.offset_index, Some(1));
        // A chosen row keeps the list load straight; shuffle follows as a
        // command (see a_chosen_row_in_a_list_never_loads_shuffled).
        assert_eq!(two.shuffle, None);
        let episode = local_load(
            &PlayRequest::tracks(vec!["spotify:episode:e".into()]),
            false,
        );
        assert_eq!(episode.context_uri, None);
        assert_eq!(episode.uris.len(), 1);
    }

    /// A plain list that plays out seeds autoplay with its last song; a
    /// stop anywhere else, a dropped session, or autoplay off does not.
    #[test]
    fn a_list_that_ends_seeds_autoplay_with_its_last_song() {
        let track = |uri: &str| {
            Some(crate::player::LocalTrack {
                uri: uri.into(),
                duration_ms: 200_000,
                ..Default::default()
            })
        };
        let playing = LocalState {
            playback: Playback::Playing,
            track: track("spotify:track:last"),
            position_ms: 198_500,
            connected: true,
            ..LocalState::default()
        };
        let stopped = LocalState {
            playback: Playback::Stopped,
            track: track("spotify:track:last"),
            connected: true,
            ..LocalState::default()
        };
        let list: Vec<String> = vec!["spotify:track:first".into(), "spotify:track:last".into()];
        assert_eq!(
            autoplay_seed(Some(&list), true, &playing, &stopped).as_deref(),
            Some("spotify:track:last")
        );
        assert_eq!(autoplay_seed(Some(&list), false, &playing, &stopped), None);
        assert_eq!(autoplay_seed(None, true, &playing, &stopped), None);
        let mid_song = LocalState {
            position_ms: 60_000,
            ..playing.clone()
        };
        assert_eq!(autoplay_seed(Some(&list), true, &mid_song, &stopped), None);
        let not_last = LocalState {
            track: track("spotify:track:first"),
            ..playing.clone()
        };
        assert_eq!(autoplay_seed(Some(&list), true, &not_last, &stopped), None);
        let dropped = LocalState {
            connected: false,
            ..stopped.clone()
        };
        assert_eq!(autoplay_seed(Some(&list), true, &playing, &dropped), None);
    }

    fn snapshot_at(percent: u8) -> LocalState {
        LocalState {
            volume: percent_to_volume(percent),
            ..LocalState::default()
        }
    }

    #[test]
    fn a_volume_set_here_is_saved_immediately() {
        let mut app = headless_app();
        app.engine.set_volume(80, true);
        assert_eq!(volume_to_percent(app.engine.settings.volume), 80);
        assert!(app.engine.settings_dirty);
    }

    #[test]
    fn a_stale_engine_snapshot_does_not_pull_the_volume_back() {
        let mut app = headless_app();
        app.engine.set_volume(80, true);

        // The engine reports `VolumeChanged` asynchronously, so its next
        // snapshot still carries the volume from before the change.
        app.handle_local(snapshot_at(20));
        assert_eq!(volume_to_percent(app.engine.local.volume), 80);
        assert_eq!(volume_to_percent(app.engine.settings.volume), 80);

        // Once it has caught up, its snapshots are trusted again.
        app.handle_local(snapshot_at(80));
        assert_eq!(volume_to_percent(app.engine.local.volume), 80);
    }

    #[test]
    fn a_volume_changed_outside_the_app_is_adopted() {
        let mut app = headless_app();
        app.handle_local(snapshot_at(35));
        assert_eq!(volume_to_percent(app.engine.local.volume), 35);
        assert_eq!(volume_to_percent(app.engine.settings.volume), 35);
    }

    /// What a Raycast script sends becomes the same action a menu pick or a
    /// media key would produce.
    #[test]
    fn a_control_command_becomes_the_action_it_names() {
        // #given
        let mut app = headless_app();
        let queue: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> = Default::default();
        app.control_commands = Some(std::sync::Arc::clone(&queue));

        // #when
        queue.lock().expect("the queue").extend([
            ControlCommand::Next,
            ControlCommand::Previous,
            ControlCommand::SeekBy(-15_000),
            ControlCommand::VolumeBy(10),
            ControlCommand::SetVolume(240),
            ControlCommand::ToggleShuffle,
            ControlCommand::Show,
        ]);
        app.handle_control_commands();

        // #then
        assert!(
            matches!(
                app.engine.actions.as_slice(),
                [
                    Action::Next,
                    Action::Previous,
                    Action::SeekBy(-15_000),
                    Action::VolumeBy(10),
                    // A percentage above the scale is clamped, not wrapped.
                    Action::SetVolume(100),
                    Action::ToggleShuffle,
                    Action::ShowWindow,
                ]
            ),
            "{:?}",
            app.engine.actions
        );
        assert!(queue.lock().expect("the queue").is_empty());
    }

    /// A play-search that finishes late never replaces a newer search or
    /// something the user started after it, and a play asked for before it
    /// never supersedes it, even within one batch of commands.
    #[test]
    fn a_late_play_search_never_overrides_a_newer_choice() {
        // #given
        let ctx = egui::Context::default();
        let mut app = headless_app();
        let queue: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> = Default::default();
        app.control_commands = Some(std::sync::Arc::clone(&queue));
        let search = |query: &str| ControlCommand::PlaySearch {
            kind: "track".to_owned(),
            query: query.to_owned(),
        };
        let found = |serial: u64, uri: &str| ApiResponse::PlaySearch {
            query: "words".to_owned(),
            serial,
            result: Ok(Some(uri.to_owned())),
        };
        let send = |app: &mut App, commands: Vec<ControlCommand>| {
            queue.lock().expect("the queue").extend(commands);
            app.handle_control_commands();
            app.apply_actions(&ctx);
            app.engine.play_search_serial
        };
        let answer = |app: &mut App, serial: u64, uri: &str| {
            app.handle_api(found(serial, uri));
            app.apply_actions(&ctx);
        };
        // The context the app last started, shown at once.
        let playing = |app: &App| {
            app.engine.assumed_context
                .as_ref()
                .map(|context| context.uri.clone())
        };

        // #when two searches are sent and the older answers last
        let first = send(&mut app, vec![search("first")]);
        let second = send(&mut app, vec![search("second")]);
        answer(&mut app, second, "spotify:track:second");
        answer(&mut app, first, "spotify:track:first");

        // #then only the newer search plays
        assert_eq!(playing(&app).as_deref(), Some("spotify:track:second"));

        // #when the user plays something before a search answers
        let third = send(&mut app, vec![search("third")]);
        send(
            &mut app,
            vec![ControlCommand::PlayUri("spotify:album:chosen".to_owned())],
        );
        answer(&mut app, third, "spotify:track:third");

        // #then the user's choice stands
        assert_eq!(playing(&app).as_deref(), Some("spotify:album:chosen"));

        // #when one batch asks for a URI and then a search
        queue.lock().expect("the queue").extend([
            ControlCommand::PlayUri("spotify:album:older".to_owned()),
            search("fourth"),
        ]);
        app.handle_control_commands();

        // #then the search takes precedence after the URI, in command order
        assert!(
            matches!(
                app.engine.actions.as_slice(),
                [Action::PlayContext { .. }, Action::PlaySearch { query, .. }] if query == "fourth"
            ),
            "{:?}",
            app.engine.actions
        );
        app.apply_actions(&ctx);
        let fourth = app.engine.play_search_serial;
        answer(&mut app, fourth, "spotify:track:fourth");

        // #then the search, asked for last, still plays
        assert_eq!(playing(&app).as_deref(), Some("spotify:track:fourth"));

        // #when a search arrives in the same frame as an older search's answer
        let fifth = send(&mut app, vec![search("fifth")]);
        queue.lock().expect("the queue").push(search("sixth"));
        app.handle_control_commands();
        app.handle_api(found(fifth, "spotify:track:fifth"));
        app.apply_actions(&ctx);
        // The serial the newer search was sent with; a stray play of the
        // older match would have moved the current one past it.
        let sixth = fifth.wrapping_add(1);
        answer(&mut app, sixth, "spotify:track:sixth");

        // #then the older answer, though current when it arrived, never plays
        assert_eq!(playing(&app).as_deref(), Some("spotify:track:sixth"));

        // #when a play waits for the engine and a search is sent meanwhile
        send(
            &mut app,
            vec![ControlCommand::PlayUri("spotify:album:waiting".to_owned())],
        );
        assert!(app.engine.queued_play.is_some(), "the engine is not connected");
        let seventh = send(&mut app, vec![search("seventh")]);
        app.engine.handle_playback(LocalPlayback::Ready {
            device_id: "local".to_owned(),
        });
        answer(&mut app, seventh, "spotify:track:seventh");

        // #then replaying the waiting play does not cancel the newer search
        assert_eq!(playing(&app).as_deref(), Some("spotify:track:seventh"));

        // #when the listener pauses, skips or steps back before a search answers
        for transport in [Action::TogglePlay, Action::Next, Action::Previous] {
            let pending = send(&mut app, vec![search("pending")]);
            app.engine.actions.push(transport.clone());
            app.apply_actions(&ctx);
            answer(&mut app, pending, "spotify:track:pending");

            // #then that choice stands
            assert_eq!(
                playing(&app).as_deref(),
                Some("spotify:track:seventh"),
                "{transport:?}"
            );
        }

        // #when the listener only changes the volume
        let louder = send(&mut app, vec![search("louder")]);
        app.engine.actions.push(Action::SetVolume(80));
        app.apply_actions(&ctx);
        answer(&mut app, louder, "spotify:track:louder");

        // #then the search still plays its match
        assert_eq!(playing(&app).as_deref(), Some("spotify:track:louder"));
    }

    /// Control clients can set state, seek, play a URI, and transfer playback.
    #[test]
    fn a_key_can_ask_for_a_state_rather_than_a_toggle() {
        // #given
        let mut app = headless_app();
        let queue: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> = Default::default();
        app.control_commands = Some(std::sync::Arc::clone(&queue));

        // #when
        queue.lock().expect("the queue").extend([
            ControlCommand::SetShuffle(true),
            ControlCommand::SetRepeat(RepeatMode::Track),
            ControlCommand::SeekTo(90_000),
            ControlCommand::PlayUri("spotify:playlist:pl1".to_owned()),
            ControlCommand::Transfer("abc123".to_owned()),
            ControlCommand::RefreshDevices,
            // Nothing is playing in a headless app, so there is no track to
            // save and this one falls away rather than erroring.
            ControlCommand::ToggleSaved,
        ]);
        app.handle_control_commands();

        // #then
        assert!(
            matches!(
                app.engine.actions.as_slice(),
                [
                    Action::SetShuffle(true),
                    Action::SetRepeat(RepeatMode::Track),
                    Action::Seek(90_000),
                    Action::PlayContext {
                        offset_uri: None,
                        offset_index: None,
                        ..
                    },
                    Action::Transfer(_),
                    Action::RefreshDevices,
                ]
            ),
            "{:?}",
            app.engine.actions
        );
        assert!(queue.lock().expect("the queue").is_empty());
    }

    /// New snapshot fields are appended so older clients keep working.
    #[test]
    fn the_snapshot_appends_what_a_key_needs_without_moving_what_was_there() {
        use crate::api::models::ArtistRef;
        // #given
        let mut app = headless_app();
        app.handle_local(LocalState {
            playback: Playback::Playing,
            track: Some(crate::player::LocalTrack {
                uri: "spotify:track:t1".to_owned(),
                title: "Go".to_owned(),
                artists: vec![ArtistRef {
                    name: "The Band".to_owned(),
                    ..ArtistRef::default()
                }],
                album: "First".to_owned(),
                art_url: Some("https://i.scdn.co/image/abc".to_owned()),
                duration_ms: 200_000,
                ..Default::default()
            }),
            position_ms: 20_000,
            volume: percent_to_volume(35),
            shuffle: true,
            repeat: RepeatMode::Track,
            ..LocalState::default()
        });

        // #when
        let snapshot = app.control_snapshot();
        let fields: Vec<&str> = snapshot.split('\t').collect();

        // #then
        assert_eq!(
            fields,
            [
                // The nine a media key or a Raycast script already read.
                "playing",
                "Go",
                "The Band",
                "First",
                "20000",
                "200000",
                "35",
                "on",
                "track",
                // The three a Stream Deck key needs, appended.
                "https://i.scdn.co/image/abc",
                // Saved state is unknown before sign-in.
                "unknown",
                // Local playback is this computer, which Spotify has not
                // named because it is not a remote device.
                "Spotifast",
            ]
        );
        // No devices seen yet is an empty array, not an empty string, so a
        // client never special-cases the answer.
        assert_eq!(
            app.control_devices_snapshot(),
            crate::single_instance::NO_DEVICES
        );
    }

    #[test]
    fn an_idle_sonos_selection_survives_spotifys_device_list() {
        let mut app = headless_app();
        app.engine.receivers = vec![crate::zeroconf::Receiver {
            name: "Office".into(),
            device_id: Some("office".into()),
            address: "192.168.1.10".parse().unwrap(),
            port: 1400,
            path: "/spotifyzc".into(),
            cast: false,
        }];
        app.engine.selected_device = Some("office".into());
        app.handle_api(ApiResponse::Devices(Ok(Vec::new())));
        assert_eq!(app.engine.selected_device.as_deref(), Some("office"));
        app.engine.selected_device = Some("departed-phone".into());
        app.handle_api(ApiResponse::Devices(Ok(Vec::new())));
        assert_eq!(app.engine.selected_device, None);
        app.engine.backend.shutdown();
    }

    #[test]
    fn restricted_remote_controls_require_a_ready_playback_session() {
        let mut app = headless_app();
        app.engine.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                item: Some(queued_song("spotify:track:song")),
                device: Some(Device {
                    id: Some("office".into()),
                    is_restricted: true,
                    supports_volume: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        for ready in [false, true] {
            app.engine.local_ready = ready;
            let now = app.engine.now_playing_live().unwrap();
            assert_eq!(now.can_control, ready);
            assert_eq!(now.can_set_volume, ready);
        }
        app.engine.remote
            .as_mut()
            .unwrap()
            .state
            .device
            .as_mut()
            .unwrap()
            .supports_volume = Some(false);
        assert!(!app.engine.now_playing_live().unwrap().can_set_volume);
        app.engine.backend.shutdown();
    }

    /// Activating a receiver can stop the music before the receiver is
    /// ready. The transfer afterwards plays if music played at the click or
    /// plays at the transfer.
    #[test]
    fn a_receiver_plays_once_ready_if_music_played_when_chosen_or_since() {
        let speaker = crate::zeroconf::Receiver {
            name: "nest audio".into(),
            device_id: Some(crate::cast::device_id("nest audio")),
            address: "192.0.2.1".parse().unwrap(),
            port: 8009,
            path: "/".into(),
            cast: true,
        };
        // Playing at the click, playing at the transfer, and the play flag.
        for (at_click, at_transfer, play) in [
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            let mut app = headless_app();
            let ctx = egui::Context::default();
            let mut now = NowPlaying {
                local: true,
                device_name: None,
                device_kind: None,
                uri: "spotify:track:1".into(),
                id: None,
                title: String::new(),
                artists: Vec::new(),
                subtitle: String::new(),
                album_name: String::new(),
                album_id: None,
                show_id: None,
                art_url: None,
                art_small: None,
                duration_ms: 200_000,
                position_ms: 1_000,
                playing: at_click,
                loading: false,
                shuffle: false,
                repeat: RepeatMode::Off,
                volume_percent: 50,
                can_control: true,
                can_set_volume: true,
                is_episode: false,
                resuming: false,
            };
            app.engine.frame_now = Some(now.clone());

            app.apply(Action::ActivateReceiver(Box::new(speaker.clone())), &ctx);
            now.playing = at_transfer;
            app.engine.frame_now = Some(now);
            app.handle_backend_events(vec![Event::ReceiverActivated {
                name: speaker.name.clone(),
                result: Ok(()),
            }]);
            app.handle_api(ApiResponse::Devices(Ok(vec![Device {
                id: speaker.device_id.clone(),
                name: speaker.name.clone(),
                ..Device::default()
            }])));

            let transfers = app.engine.backend.take_transfer_requests();
            assert!(
                matches!(
                    transfers.as_slice(),
                    [ApiRequest::Transfer { device_id, play: sent }]
                        if Some(device_id) == speaker.device_id.as_ref() && *sent == play
                ),
                "{at_click} {at_transfer}: {transfers:?}"
            );
            app.engine.backend.shutdown();
        }
    }

    /// Another Spotify device with the receiver's name does not take over.
    #[test]
    fn a_receiver_takes_over_by_its_own_id_when_names_clash() {
        let mut app = headless_app();
        let speaker = crate::zeroconf::Receiver {
            name: "nest audio".into(),
            device_id: Some(crate::cast::device_id("nest audio")),
            address: "192.0.2.1".parse().unwrap(),
            port: 8009,
            path: "/".into(),
            cast: true,
        };
        app.engine.receivers = vec![speaker.clone()];
        app.handle_backend_events(vec![Event::ReceiverActivated {
            name: speaker.name.clone(),
            result: Ok(()),
        }]);
        app.handle_api(ApiResponse::Devices(Ok(vec![
            Device {
                id: Some("phone".into()),
                name: speaker.name.clone(),
                ..Device::default()
            },
            Device {
                id: speaker.device_id.clone(),
                name: speaker.name.clone(),
                ..Device::default()
            },
        ])));

        let transfers = app.engine.backend.take_transfer_requests();
        assert!(
            matches!(
                transfers.as_slice(),
                [ApiRequest::Transfer { device_id, .. }] if Some(device_id) == speaker.device_id.as_ref()
            ),
            "{transfers:?}"
        );
        app.engine.backend.shutdown();
    }

    /// Spotify device responses update the control snapshot.
    #[test]
    fn a_device_list_reaches_the_slot_when_spotify_answers() {
        // #given
        let mut app = headless_app();
        let slot = std::sync::Arc::new(std::sync::Mutex::new(
            crate::single_instance::NO_DEVICES.to_owned(),
        ));
        app.control_devices = Some(std::sync::Arc::clone(&slot));
        app.engine.control_devices_stale = false;

        // #when
        app.handle_api(ApiResponse::Devices(Ok(vec![Device {
            id: Some("abc123".to_owned()),
            name: "Kitchen\tspeaker".to_owned(),
            kind: "Speaker".to_owned(),
            is_active: true,
            ..Device::default()
        }])));
        app.sync_media_controls(&egui::Context::default());

        // #then
        let written = slot.lock().expect("the slot").clone();
        assert_eq!(
            written,
            r#"[{"active":true,"id":"abc123","kind":"Speaker","name":"Kitchen\tspeaker"}]"#,
            "a name is carried whole, tab and all, because JSON escapes it \
             where the tab-separated snapshot could not"
        );
        // Written once and not again until the next answer.
        assert!(!app.engine.control_devices_stale);
    }

    /// `play` and `pause` say what state to end in, so the one that would
    /// undo the current state does nothing.
    #[test]
    fn play_and_pause_do_not_toggle_the_wrong_way() {
        let mut app = headless_app();
        let queue: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> = Default::default();
        app.control_commands = Some(std::sync::Arc::clone(&queue));

        // Nothing is playing in a headless app, so `pause` has nothing to do
        // and `play` asks for the toggle.
        queue
            .lock()
            .expect("the queue")
            .extend([ControlCommand::Pause, ControlCommand::Play]);
        app.handle_control_commands();

        assert!(
            matches!(app.engine.actions.as_slice(), [Action::TogglePlay]),
            "{:?}",
            app.engine.actions
        );
    }

    /// A skin with a bitmap in it, for pretending one was read.
    fn some_skin(name: &str) -> crate::skin::Skin {
        let image = image::RgbImage::from_pixel(275, 116, image::Rgb([9, 9, 9]));
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let archive = crate::skin::zip::write(&[("main.bmp", png.get_ref(), false)]);
        crate::skin::Skin::from_archive(name, &archive).unwrap()
    }

    #[test]
    fn a_skin_read_late_does_not_override_a_newer_choice() {
        let mut app = headless_app();
        app.engine.settings.winamp_window = true;
        app.engine.settings.skin = Some("B.wsz".into());
        app.skin_loaded(crate::winamp::Loaded {
            name: "A.wsz".into(),
            result: Ok(some_skin("A")),
            installed: false,
        });
        assert_eq!(app.winamp.worn.as_deref(), Some("A.wsz"));
        assert_eq!(app.engine.settings.skin.as_deref(), Some("B.wsz"));
    }

    #[test]
    fn a_dropped_skin_becomes_the_choice_and_a_failed_one_is_forgotten() {
        let mut app = headless_app();
        app.engine.settings.winamp_window = true;
        app.skin_loaded(crate::winamp::Loaded {
            name: "Dropped.wsz".into(),
            result: Ok(some_skin("Dropped")),
            installed: true,
        });
        assert_eq!(app.engine.settings.skin.as_deref(), Some("Dropped.wsz"));
        assert_eq!(app.winamp.worn.as_deref(), Some("Dropped.wsz"));
        assert!(
            app.engine.toasts
                .iter()
                .any(|toast| toast.message == "Added Dropped skin")
        );

        app.engine.settings.skin = Some("Gone.wsz".into());
        app.skin_loaded(crate::winamp::Loaded {
            name: "Gone.wsz".into(),
            result: Err(crate::skin::SkinError::Empty),
            installed: false,
        });
        assert_eq!(app.engine.settings.skin.as_deref(), Some("Dropped.wsz"));
        assert_eq!(app.winamp.worn.as_deref(), Some("Dropped.wsz"));
        assert!(
            app.engine.toasts
                .iter()
                .any(|toast| toast.message.starts_with("Gone: "))
        );
    }

    /// A link from outside waits for the account and then opens its page;
    /// a song's link opens the album the song is on.
    #[test]
    fn a_link_waits_for_the_account_and_then_opens_its_page() {
        use crate::api::models::{Album, Track};

        // #given a signed-out app handed a playlist link
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.engine.actions
            .push(Action::OpenLink("spotify:playlist:pl1".into()));
        app.apply_actions(&ctx);

        // #then the window is asked for but the page waits
        assert_eq!(*app.engine.page(), Page::Home);
        assert_eq!(app.engine.pending_link.as_deref(), Some("spotify:playlist:pl1"));

        // #when the account arrives
        app.engine.user = Some(User {
            id: "me".into(),
            ..User::default()
        });
        app.engine.open_pending_link();

        // #then the playlist opens, once
        assert_eq!(*app.engine.page(), Page::Playlist("pl1".into()));
        assert_eq!(app.engine.pending_link, None);

        // #when a song whose album is known is linked
        app.engine.track_cache.insert(
            "t1".into(),
            Track {
                id: Some("t1".into()),
                uri: "spotify:track:t1".into(),
                album: Some(Album {
                    id: "al1".into(),
                    ..Album::default()
                }),
                ..Track::default()
            },
        );
        app.engine.actions
            .push(Action::OpenLink("spotify:track:t1".into()));
        app.apply_actions(&ctx);

        // #then its album's page opens
        assert_eq!(*app.engine.page(), Page::Album("al1".into()));
        assert_eq!(app.engine.pending_link, None);

        // #when a song still unknown is linked
        app.engine.actions
            .push(Action::OpenLink("spotify:track:t2".into()));
        app.apply_actions(&ctx);

        // #then the link waits for Spotify's answer rather than guessing
        assert_eq!(*app.engine.page(), Page::Album("al1".into()));
        assert_eq!(app.engine.pending_link.as_deref(), Some("spotify:track:t2"));
        assert!(app.engine.track_requests.contains("t2"));

        // #when Spotify has no such song
        app.handle_api(ApiResponse::Track {
            id: "t2".into(),
            result: Err(crate::api::client::ApiError::Status {
                status: 404,
                message: "not found".into(),
            }),
        });

        // #then the link is given up on and the user told
        assert_eq!(app.engine.pending_link, None);
        assert!(
            app.engine.toasts
                .iter()
                .any(|toast| toast.message.contains("Cannot open"))
        );
    }

    /// A link to something the app has no page for is refused with a word,
    /// not held forever.
    #[test]
    fn a_link_to_nothing_the_app_shows_says_so() {
        // #given
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.engine.user = Some(User {
            id: "me".into(),
            ..User::default()
        });

        // #when
        app.engine.actions
            .push(Action::OpenLink("spotify:station:track:t1".into()));
        app.apply_actions(&ctx);

        // #then
        assert_eq!(app.engine.pending_link, None);
        assert_eq!(*app.engine.page(), Page::Home);
        assert!(
            app.engine.toasts
                .iter()
                .any(|toast| toast.message.contains("cannot open"))
        );
    }

    #[test]
    fn search_links_wait_for_sign_in_and_open_the_latest_query_once() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        for link in [
            "https://open.spotify.com/search/old",
            "https://open.spotify.com/search/artist%3ABj%C3%B6rk",
        ] {
            app.engine.open_link(crate::link::parse(link).unwrap());
            app.apply_actions(&ctx);
        }
        assert_eq!(*app.engine.page(), Page::Home);
        assert!(app.engine.search.query.is_empty());
        assert!(app.engine.pending_link.is_some());

        app.engine.user = Some(User {
            id: "me".into(),
            ..User::default()
        });
        app.engine.open_pending_link();
        app.apply_actions(&ctx);
        assert_eq!(*app.engine.page(), Page::Search);
        assert_eq!(app.engine.search.query, "artist:Björk");
        assert_eq!(app.engine.search.committed, "artist:Björk");
        assert!(app.engine.search.focus_requested);
        assert!(app.engine.pending_link.is_none());
        assert!(app.engine.now_playing().is_none());
        let serial = app.engine.search.serial;
        app.engine.open_pending_link();
        app.apply_actions(&ctx);
        assert_eq!(app.engine.search.serial, serial);

        app.engine.open_link(crate::link::parse("https://open.spotify.com/search").unwrap());
        app.apply_actions(&ctx);
        assert_eq!(*app.engine.page(), Page::Search);
        assert!(app.engine.search.query.is_empty());
        assert!(app.engine.search.committed.is_empty());
        assert!(matches!(app.engine.search.results, Loadable::NotLoaded));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mpris_search_links_open_search_on_a_private_bus() {
        use std::time::{Duration, Instant};
        const CHILD: &str = "SPOTIFAST_SEARCH_PRIVATE_BUS";
        if std::env::var_os(CHILD).is_none() {
            let root = std::env::temp_dir().join(format!(
                "spotifast-search-bus-{:016x}",
                rand::random::<u64>()
            ));
            std::fs::create_dir(&root).unwrap();
            let config = root.join("session.conf");
            std::fs::write(&config, r#"<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth><policy context="default"><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy></busconfig>"#).unwrap();
            let result = std::process::Command::new("dbus-run-session")
                .arg("--config-file")
                .arg(config)
                .arg("--")
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::tests::mpris_search_links_open_search_on_a_private_bus",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            std::fs::remove_dir_all(root).unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let mut app = headless_app();
        app.engine.user = Some(User {
            id: "me".into(),
            ..User::default()
        });
        app.media_controls = Some(MediaControls::start(media_app(), || {}));
        let client = zbus::blocking::connection::Builder::session()
            .unwrap()
            .method_timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let call = |uri: &str| {
            client.call_method(
                Some("org.mpris.MediaPlayer2.spotifast"),
                "/org/mpris/MediaPlayer2",
                Some("org.mpris.MediaPlayer2.Player"),
                "OpenUri",
                &(uri,),
            )
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Err(error) = call("https://open.spotify.com/search/here%20comes%20the%20sun") {
            assert!(Instant::now() < deadline, "MPRIS did not start: {error}");
            std::thread::sleep(Duration::from_millis(10));
        }
        let wait_for_command = |app: &mut App| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.engine.actions.is_empty() {
                app.handle_media_commands();
                assert!(
                    Instant::now() < deadline,
                    "MPRIS command did not reach the app"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        wait_for_command(&mut app);
        assert!(matches!(app.engine.actions.as_slice(), [Action::OpenLink(_)]));
        let schemes: Vec<String> = zbus::blocking::Proxy::new(
            &client,
            "org.mpris.MediaPlayer2.spotifast",
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2",
        )
        .unwrap()
        .get_property("SupportedUriSchemes")
        .unwrap();
        assert!(schemes.iter().any(|scheme| scheme == "https"));
        app.apply_actions(&egui::Context::default());
        assert_eq!(*app.engine.page(), Page::Search);
        assert_eq!(app.engine.search.query, "here comes the sun");
        assert_eq!(app.engine.search.committed, "here comes the sun");
        assert!(app.engine.now_playing().is_none());

        call("spotify:playlist:unchanged").unwrap();
        wait_for_command(&mut app);
        assert!(
            matches!(app.engine.actions.as_slice(), [Action::PlayContext { uri, offset_uri: None, offset_index: None }] if uri == "spotify:playlist:unchanged")
        );
    }

    /// A playlist shared by invitation takes songs once Spotify's rootlist
    /// says so, though the Web API calls it neither owned nor collaborative.
    #[test]
    fn a_playlist_shared_by_invitation_takes_songs() {
        // #given a friend's playlist in the library
        let mut app = headless_app();
        app.engine.user = Some(User {
            id: "me".into(),
            ..User::default()
        });
        let theirs = Playlist {
            id: "shared".into(),
            name: "the Best Music Ever".into(),
            uri: "spotify:playlist:shared".into(),
            owner: crate::api::models::Owner {
                id: Some("friend".into()),
                ..Default::default()
            },
            ..Playlist::default()
        };
        let mut mine = theirs.clone();
        mine.id = "mine".into();
        mine.uri = "spotify:playlist:mine".into();
        mine.owner.id = Some("me".into());
        let mut public = theirs.clone();
        public.id = "public".into();
        public.uri = "spotify:playlist:public".into();
        app.engine.library.playlists = Loadable::Loaded(vec![theirs.clone(), mine.clone(), public]);

        // #then only the account's own takes songs before Spotify's word
        assert!(app.engine.can_edit_playlist(&mine));
        assert!(!app.engine.can_edit_playlist(&theirs));
        assert_eq!(
            app.engine.editable_playlists(),
            vec![("mine".to_string(), "the Best Music Ever".to_string())]
        );

        // #when the rootlist names the friend's playlist among the editable
        app.handle_backend_events(vec![Event::Rootlist {
            result: Ok(crate::player::Rootlist {
                entries: vec![crate::player::RootlistEntry::Playlist(
                    "spotify:playlist:shared".into(),
                )],
                editable: ["spotify:playlist:shared".to_string()]
                    .into_iter()
                    .collect(),
            }),
        }]);

        // #then it takes songs, and the followed public one still does not
        assert!(app.engine.can_edit_playlist(&theirs));
        let editable: Vec<String> = app.engine.editable_playlists()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(editable, ["shared", "mine"]);
    }

    /// The player bar menu offers removal only while the playing track
    /// comes from a playlist the account may edit.
    #[test]
    fn player_bar_menu_knows_its_editable_playlist() {
        // #given an owned playlist, a friend's, and one known by its page
        let mut app = headless_app();
        app.engine.user = Some(User {
            id: "me".into(),
            ..User::default()
        });
        let mine = Playlist {
            id: "mine".into(),
            name: "Mine".into(),
            uri: "spotify:playlist:mine".into(),
            owner: crate::api::models::Owner {
                id: Some("me".into()),
                ..Default::default()
            },
            snapshot_id: Some("snap1".into()),
            ..Playlist::default()
        };
        let theirs = Playlist {
            id: "theirs".into(),
            name: "Theirs".into(),
            uri: "spotify:playlist:theirs".into(),
            owner: crate::api::models::Owner {
                id: Some("friend".into()),
                ..Default::default()
            },
            ..Playlist::default()
        };
        app.engine.library.playlists = Loadable::Loaded(vec![mine.clone(), theirs]);
        app.engine.playlist_pages.insert(
            "paged".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "paged".into(),
                    uri: "spotify:playlist:paged".into(),
                    owner: crate::api::models::Owner {
                        id: Some("me".into()),
                        ..Default::default()
                    },
                    snapshot_id: Some("snap9".into()),
                    ..Playlist::default()
                }),
                ..Default::default()
            },
        );
        let assume = |app: &mut App, uri: &str| {
            app.engine.assumed_context = Some(AssumedContext {
                uri: uri.into(),
                shuffle: None,
                at: Instant::now(),
            });
        };

        // #when the owned playlist plays, its snapshot rides along
        assume(&mut app, "spotify:playlist:mine");
        assert_eq!(
            app.engine.editable_context_playlist(),
            Some(RowContext::Context {
                uri: "spotify:playlist:mine".into(),
                editable_playlist: Some(("mine".into(), Some("snap1".into()))),
            })
        );

        // #when the page alone knows the playlist, it still counts
        assume(&mut app, "spotify:playlist:paged");
        assert_eq!(
            app.engine.editable_context_playlist(),
            Some(RowContext::Context {
                uri: "spotify:playlist:paged".into(),
                editable_playlist: Some(("paged".into(), Some("snap9".into()))),
            })
        );

        // #then anything else stays quiet: a friend's list, an album,
        // Liked Songs, radio, and an unknown playlist
        for uri in [
            "spotify:playlist:theirs",
            "spotify:album:alb1",
            "spotify:user:me:collection",
            "spotify:station:track:xyz",
            "spotify:playlist:unloaded",
        ] {
            assume(&mut app, uri);
            assert_eq!(app.engine.editable_context_playlist(), None, "{uri}");
        }
        app.engine.assumed_context = None;
        assert_eq!(app.engine.editable_context_playlist(), None, "no context");
    }

    /// Spotify's own apps reorder playlists while Spotifast is in the
    /// background (#683); coming back reads the tree again, at most every
    /// `ROOTLIST_REFRESH`, once every playlist has arrived.
    #[test]
    fn returning_to_the_window_reads_the_playlist_tree_again() {
        let mut app = test_app("rootlist-focus");
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        let reads = |app: &mut App, focused: bool| {
            let before = app.engine.rootlist_read_at;
            let wait = app.window_focus(Some(focused));
            (app.engine.rootlist_read_at != before, wait)
        };
        app.window_focus(Some(false));
        assert_eq!(reads(&mut app, true), (false, None), "playlists loading");
        app.engine.library.playlists = Loadable::Loaded(Vec::new());
        app.engine.library.playlists_next = Some(50);
        assert_eq!(reads(&mut app, true), (false, None), "more to come");
        app.engine.load_more(Page::Home);
        assert_eq!(app.engine.library.playlists_asked, Some(50));
        assert_eq!(reads(&mut app, true), (false, None), "page on its way");
        app.engine.library.playlists_asked = None;
        // The first read clears the request.
        app.engine.read_rootlist();
        assert_eq!(reads(&mut app, true), (false, None), "already read");

        // Coming back soon after a read waits for the rest of the interval,
        // then reads while the window stays focused.
        app.window_focus(Some(false));
        let (read, wait) = reads(&mut app, true);
        assert!(!read);
        assert!(wait.is_some_and(|wait| wait <= ROOTLIST_REFRESH));
        app.engine.rootlist_read_at = Some(Instant::now() - ROOTLIST_REFRESH);
        assert_eq!(reads(&mut app, true), (true, None));
        assert_eq!(reads(&mut app, true), (false, None), "staying focused");

        // A frame that does not say whether it has focus, such as one run
        // without a window, is no return.
        app.engine.rootlist_read_at = Some(Instant::now() - ROOTLIST_REFRESH);
        app.window_focus(Some(false));
        assert_eq!(app.window_focus(None), None);
        assert!(!app.engine.rootlist_wanted, "no word on focus");

        // A return long after the last read reads at once.
        app.engine.rootlist_read_at = Some(Instant::now() - ROOTLIST_REFRESH);
        app.window_focus(Some(false));
        assert_eq!(reads(&mut app, true), (true, None));

        // Closing to the tray and showing the window again is a return,
        // even when it had focus as it closed; hidden frames read nothing.
        app.engine.rootlist_read_at = Some(Instant::now() - ROOTLIST_REFRESH);
        app.window_gone();
        assert_eq!(reads(&mut app, true), (false, None), "hidden");
        app.window_hidden = false;
        assert_eq!(reads(&mut app, true), (true, None), "shown again");

        app.engine.auth = AuthStatus::SignedOut;
        app.engine.rootlist_read_at = Some(Instant::now() - ROOTLIST_REFRESH);
        app.window_focus(Some(false));
        assert_eq!(reads(&mut app, true), (false, None), "signed out");
        app.engine.backend.shutdown();
    }

    /// Folder order survives a restart, but only for the account that
    /// supplied it; edit grants wait for a fresh session answer.
    #[test]
    fn the_last_playlist_tree_stays_visible_for_its_account() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-rootlist-restart-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let options = || AppOptions {
            media_controls: false,
            restore_sign_in: false,
            tray: false,
        };
        let entries = vec![
            crate::player::RootlistEntry::FolderStart {
                id: "folder".into(),
                name: "Favorites".into(),
            },
            crate::player::RootlistEntry::Playlist("spotify:playlist:one".into()),
            crate::player::RootlistEntry::FolderEnd,
        ];
        let mut app = App::new(
            &Waker::default(),
            dirs.clone(),
            Settings::default(),
            options(),
        );
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.engine.user = Some(User {
            id: "listener".into(),
            ..User::default()
        });
        app.handle_backend_events(vec![Event::Rootlist {
            result: Ok(crate::player::Rootlist {
                entries: entries.clone(),
                editable: ["spotify:playlist:one".to_string()].into_iter().collect(),
            }),
        }]);
        assert!(app.engine.session_dirty);
        app.save_session();
        app.engine.flush_files();
        drop(app);

        let mut restored = App::new(
            &Waker::default(),
            dirs.clone(),
            Settings::default(),
            options(),
        );
        restored.handle_api(ApiResponse::Me(Ok(User {
            id: "someone-else".into(),
            ..User::default()
        })));
        assert!(
            restored.engine.rootlist.is_empty(),
            "another account sees no cached tree"
        );
        restored.handle_api(ApiResponse::Me(Ok(User {
            id: "listener".into(),
            ..User::default()
        })));
        assert_eq!(restored.engine.rootlist, entries);
        assert!(
            restored.engine.editable_by_grant.is_empty(),
            "cached order must not cache a stale edit grant"
        );
        restored.engine.handle_auth(AuthStatus::SignedOut);
        assert!(restored.engine.rootlist.is_empty());
        assert_eq!(
            restored.engine.rootlist_cache, None,
            "signing out removes the account's cached tree"
        );
        drop(restored);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_closing_main_window_never_takes_the_mini_players_size() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.attach(&ctx);
        app.engine.actions.push(Action::ToggleWinampWindow);
        app.apply_actions(&ctx);
        assert!(app.switch_intent && app.engine.settings.winamp_window);

        // Native close events may leave another UI frame to draw. It still
        // belongs to the main window, whose geometry eframe will save.
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.frame_ui(ui));
        output.textures_delta.clear();
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            !commands.iter().any(|command| matches!(
                command,
                egui::ViewportCommand::InnerSize(_)
                    | egui::ViewportCommand::MinInnerSize(_)
                    | egui::ViewportCommand::MaxInnerSize(_)
                    | egui::ViewportCommand::Maximized(_)
            )),
            "the retiring main window must keep its geometry: {commands:?}"
        );
    }

    /// Switching from Winamp back to the main window preserves the main
    /// window's size and position across the closing mini-window frame,
    /// and the mini player's position across the main window's.
    #[test]
    fn closing_winamp_frame_does_not_overwrite_main_window_geometry() {
        let mut app = headless_app();
        let main = fastframe_shell::window::Geometry {
            size: Some([1024.0, 768.0]),
            position: Some([100.0, 150.0]),
            maximized: false,
        };
        app.window_geometry = main;
        let frame = |app: &mut App, ctx: &egui::Context, rect: egui::Rect| {
            let mut raw_input = egui::RawInput::default();
            let viewport = raw_input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default();
            viewport.inner_rect = Some(rect);
            viewport.outer_rect = Some(rect);
            let mut output = ctx.run_ui(raw_input, |ui| {
                app.frame_ui(ui);
                app.remember_geometry(ui.ctx());
            });
            output.textures_delta.clear();
        };

        // Toggle from main window to Winamp window
        let ctx = egui::Context::default();
        app.engine.actions.push(Action::ToggleWinampWindow);
        app.apply_actions(&ctx);
        assert!(app.engine.settings.winamp_window);
        assert!(app.switch_intent);

        // The main window's closing frame is not the mini player's position.
        let big = egui::Rect::from_min_size(egui::pos2(100.0, 150.0), egui::vec2(1024.0, 768.0));
        frame(&mut app, &ctx, big);
        assert_eq!(app.winamp.geometry.position, None);

        // Attach the Winamp window, which follows its own position.
        let mini_ctx = egui::Context::default();
        app.attach(&mini_ctx);
        assert!(!app.switch_intent);
        let mini_rect = egui::Rect::from_min_size(egui::pos2(50.0, 50.0), egui::vec2(275.0, 116.0));
        frame(&mut app, &mini_ctx, mini_rect);
        assert_eq!(app.winamp.geometry.position, Some([50.0, 50.0]));
        assert_eq!(app.window_geometry, main);

        // Switch back: the closing mini frame arms switch_intent, which
        // keeps its tiny rectangle out of the main window's geometry.
        app.engine.actions.push(Action::ToggleWinampWindow);
        frame(&mut app, &mini_ctx, mini_rect);
        assert!(!app.engine.settings.winamp_window);
        assert!(app.switch_intent);
        assert_eq!(app.window_geometry, main);

        // The new main window opens from it (entrypoint's native_options),
        // so attaching sends no size or position after the fact.
        let main_ctx = egui::Context::default();
        let mut output = main_ctx.run_ui(Default::default(), |_ui| {
            app.attach(&main_ctx);
        });
        output.textures_delta.clear();
        let commands = &output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("the root viewport")
            .commands;
        assert!(
            !commands.iter().any(|command| matches!(
                command,
                egui::ViewportCommand::InnerSize(_) | egui::ViewportCommand::OuterPosition(_)
            )),
            "{commands:?}"
        );
        assert_eq!(app.window_geometry, main);
        app.engine.backend.shutdown();
    }

    fn cached_liked_app() -> App {
        use crate::api::models::SavedTrack;
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.engine.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.engine.liked_songs.cache_checked = true;
        app.engine.load_generation = 1;
        app.engine.liked_songs.start_refresh(1);
        app.engine.liked_songs.absorb(
            0,
            crate::api::models::Page {
                items: (0..100)
                    .map(|n| SavedTrack {
                        track: Track {
                            uri: format!("spotify:track:{n}"),
                            id: Some(n.to_string()),
                            name: format!("Song {n}"),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .collect(),
                total: 100,
                limit: 100,
                ..Default::default()
            },
            jiff::Timestamp::now().as_second(),
        );
        app.engine.sync_liked_songs();
        app
    }

    #[test]
    fn liked_cache_ignores_another_account_and_late_responses_after_sign_out() {
        let mut app = cached_liked_app();
        let cache = app.engine.liked_songs.checkpoint("alice".into(), true).unwrap();
        app.engine.reset_data();
        app.engine.user = Some(User {
            id: "bob".into(),
            ..Default::default()
        });
        app.engine.ensure_liked_songs();
        let generation = app.engine.liked_songs.generation;
        app.engine.receive_liked_cache("alice", generation, Some(cache.clone()));
        assert!(app.engine.library.liked.items.is_empty());
        app.handle_api(ApiResponse::SavedTracks {
            offset: 0,
            generation,
            account_id: Some("alice".into()),
            result: Ok(crate::api::models::Page {
                items: vec![crate::api::models::SavedTrack::default()],
                ..Default::default()
            }),
        });
        assert!(app.engine.library.liked.items.is_empty());
        app.engine.reset_data();
        app.engine.user = None;
        app.engine.receive_liked_cache("bob", generation, Some(cache));
        assert!(!app.engine.liked_songs.cache_checked);
        assert!(app.engine.library.liked.items.is_empty());
    }

    #[test]
    fn liked_rows_added_page_by_page_match_rows_made_at_once() {
        use crate::api::models::SavedTrack;
        let mut app = headless_app();
        app.engine.backend.set_offline(true);
        app.engine.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.engine.liked_songs.cache_checked = true;
        app.engine.liked_songs.generation = 1;
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let song = |n: u32| SavedTrack {
            track: Track {
                uri: format!("spotify:track:{n}"),
                name: format!("Song {n}"),
                ..Default::default()
            },
            ..Default::default()
        };
        let pages = 6;
        for page in 0..pages {
            let offset = page * 50;
            app.handle_api(ApiResponse::SavedTracks {
                offset,
                generation: 1,
                account_id: Some("alice".into()),
                result: Ok(crate::api::models::Page {
                    items: (offset..offset + 50).map(song).collect(),
                    total: pages * 50,
                    limit: 50,
                    offset,
                    next: (page + 1 < pages).then(|| "next".into()),
                }),
            });
            if page == 3 {
                // An unlike between pages; through the model, since a click
                // would also start a refresh of a list this old.
                app.engine.liked_songs
                    .change("spotify:track:20".into(), false, song(20).track);
                app.engine.sync_liked_songs();
            }
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                crate::ui::collection::liked(&mut app, ui)
            });
            output.textures_delta.clear();
            let shown: Vec<String> = app.engine.table_rows[&Page::LikedSongs]
                .items
                .iter()
                .map(|(item, _, _)| item.uri().to_string())
                .collect();
            let expected: Vec<String> = app.engine.library
                .liked
                .items
                .iter()
                .map(|saved| saved.track.uri.clone())
                .collect();
            assert_eq!(shown, expected, "after page {page}");
        }
        assert_eq!(app.engine.library.liked.items.len(), 299);
        app.engine.backend.shutdown();
    }

    #[test]
    fn liking_and_unliking_update_rows_before_the_network_answers() {
        let mut app = cached_liked_app();
        app.engine.set_saved("spotify:track:0".into(), false);
        assert_eq!(app.engine.library.liked.items.len(), 99);
        assert!(
            !app.engine.library
                .liked
                .items
                .iter()
                .any(|item| item.track.uri == "spotify:track:0")
        );
        app.engine.set_saved("spotify:track:new".into(), true);
        assert_eq!(app.engine.library.liked.items.len(), 100);
        assert_eq!(app.engine.library.liked.items[0].track.uri, "spotify:track:new");
        app.handle_api(ApiResponse::SavedChanged {
            uris: vec!["spotify:track:new".into()],
            saved: true,
            result: Ok(()),
        });
        assert_eq!(
            app.engine.library.liked.items.len(),
            100,
            "acknowledging a like does not clear the list"
        );
        assert_eq!(app.engine.library.liked.items[0].track.uri, "spotify:track:new");
        app.handle_api(ApiResponse::Contains {
            uris: vec!["spotify:track:new".into()],
            result: Ok(vec![false]),
        });
        assert_eq!(app.engine.is_saved("spotify:track:new"), Some(true));
    }

    #[test]
    fn manual_liked_refresh_keeps_rows_sort_and_selection_while_loading() {
        let mut app = cached_liked_app();
        let sort = TableSort {
            column: SortColumn::Title,
            ascending: false,
        };
        app.engine.table_sorts.insert(Page::LikedSongs, sort);
        app.engine.pick_row(&Page::LikedSongs, "liked", 7, RowPick::Only, 100);
        let revision = app.engine.library.liked.revision;
        app.engine.refresh_liked_songs();
        assert_eq!(app.engine.library.liked.items.len(), 100);
        assert_eq!(app.engine.library.liked.revision, revision);
        assert_eq!(app.engine.table_sorts[&Page::LikedSongs], sort);
        assert_eq!(picked(&app, &Page::LikedSongs), vec![7]);
        assert!(app.engine.library.liked.loading);
        app.handle_api(ApiResponse::SavedTracks {
            offset: 0,
            generation: 1,
            account_id: Some("alice".into()),
            result: Ok(crate::api::models::Page::default()),
        });
        assert_eq!(
            app.engine.library.liked.items.len(),
            100,
            "a previous load cannot replace current rows"
        );
    }

    /// Unliking a song shortens Liked Songs. The table caches its row order
    /// against the list's revision, so a silent removal leaves the cache
    /// pointing past the end of the rows and the next frame panics.
    #[test]
    fn unliking_a_song_moves_the_liked_revision() {
        use crate::api::models::SavedTrack;

        let saved = |uri: &str| SavedTrack {
            added_at: None,
            track: Track {
                uri: uri.into(),
                ..Default::default()
            },
        };
        let mut app = headless_app();
        app.engine.library.liked.items = vec![saved("spotify:track:stays"), saved("spotify:track:goes")];
        app.engine.library.liked.total = Some(2);
        app.engine.library.liked.loaded_once = true;
        let before = app.engine.library.liked.revision;

        app.handle_api(ApiResponse::SavedChanged {
            uris: vec!["spotify:track:goes".into()],
            saved: false,
            result: Ok(()),
        });

        let uris: Vec<&str> = app.engine.library
            .liked
            .items
            .iter()
            .map(|item| item.track.uri.as_str())
            .collect();
        assert_eq!(uris, vec!["spotify:track:stays"], "the song is gone");
        assert_eq!(app.engine.library.liked.total, Some(1));
        assert_ne!(
            app.engine.library.liked.revision, before,
            "the shorter list must invalidate the table's cached row order"
        );
    }

    #[test]
    fn offline_ensure_loaded_suppresses_network_requests() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.engine.network_reachable = false;
        app.engine.ensure_loaded(Page::Playlist("offline_playlist".into()));

        assert!(app.engine.backend.take_playlist_item_requests().is_empty());
        assert_eq!(
            app.engine.playlist_pages["offline_playlist"].playlist,
            Loadable::NotLoaded
        );
        assert!(!app.engine.playlist_pages["offline_playlist"].items.loading);
    }

    #[test]
    fn offline_cached_playlist_preserves_content() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        let page = PlaylistPage {
            playlist: Loadable::Loaded(Playlist {
                id: "cached_pl".into(),
                name: "Cached Playlist".into(),
                ..Default::default()
            }),
            items: PagedList {
                items: vec![cached_playlist_row("spotify:track:cached_song")],
                total: Some(1),
                loaded_once: true,
                ..Default::default()
            },
            ..Default::default()
        };
        app.engine.playlist_pages.insert("cached_pl".into(), page);
        app.engine.network_reachable = false;

        app.engine.ensure_loaded(Page::Playlist("cached_pl".into()));

        assert!(app.engine.backend.take_playlist_item_requests().is_empty());
        assert_eq!(app.engine.playlist_pages["cached_pl"].items.items.len(), 1);
        assert!(app.engine.playlist_pages["cached_pl"].playlist.get().is_some());
    }

    #[test]
    fn offline_load_more_is_suppressed() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        let page = PlaylistPage {
            items: PagedList {
                items: vec![],
                total: Some(100),
                next_offset: Some(50),
                loaded_once: true,
                ..Default::default()
            },
            ..Default::default()
        };
        app.engine.playlist_pages.insert("cached_pl".into(), page);
        app.engine.network_reachable = false;

        app.engine.load_more(Page::Playlist("cached_pl".into()));

        assert!(app.engine.backend.take_playlist_item_requests().is_empty());
        assert!(!app.engine.playlist_pages["cached_pl"].items.loading);
    }

    #[test]
    fn network_restored_recovers_incomplete_active_page() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.engine.network_reachable = false;
        app.engine.open(Page::Playlist("incomplete_pl".into()));

        assert!(app.engine.backend.take_playlist_item_requests().is_empty());

        app.handle_backend_events(vec![Event::NetworkStatus(true)]);

        assert!(app.engine.network_reachable);
        assert_eq!(app.engine.backend.take_playlist_item_requests().len(), 1);
    }

    #[test]
    fn network_restored_does_not_refetch_complete_cached_page() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        let page = PlaylistPage {
            playlist: Loadable::Loaded(Playlist {
                id: "complete_pl".into(),
                ..Default::default()
            }),
            items: PagedList {
                items: vec![cached_playlist_row("spotify:track:1")],
                total: Some(1),
                loaded_once: true,
                next_offset: None,
                ..Default::default()
            },
            ..Default::default()
        };
        app.engine.playlist_pages.insert("complete_pl".into(), page);
        app.engine.open(Page::Playlist("complete_pl".into()));
        let _ = app.engine.backend.take_playlist_item_requests();

        app.engine.network_reachable = false;
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);

        assert!(app.engine.network_reachable);
        assert!(
            app.engine.backend.take_playlist_item_requests().is_empty(),
            "complete cached page must not be refetched on network restoration"
        );
        assert_eq!(app.engine.playlist_pages["complete_pl"].items.items.len(), 1);
    }

    #[test]
    fn duplicate_network_reachability_events_do_not_refetch() {
        let mut app = headless_app();
        app.engine.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.engine.open(Page::Playlist("incomplete_pl".into()));
        app.engine.network_reachable = true;
        let _ = app.engine.backend.take_playlist_item_requests();

        // Already reachable: duplicate reachability event must not trigger recovery
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);

        assert!(app.engine.backend.take_playlist_item_requests().is_empty());
    }

    /// The system's word on reaching the internet (fastframe-power, on
    /// macOS) goes to the backend once per change, and not at all where the
    /// app does not follow it.
    #[test]
    fn the_internet_coming_and_going_reaches_the_backend_once_per_change() {
        use crate::power::{Conditions, Power};
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.power = Power::fixed(Conditions::default());
        app.follows_reachability = true;
        app.sync_power(&ctx);
        assert!(app.engine.backend.take_network_statuses().is_empty(), "online");

        let offline = Conditions {
            online: false,
            ..Conditions::default()
        };
        app.power.set_conditions(offline);
        app.sync_power(&ctx);
        app.sync_power(&ctx);
        assert_eq!(app.engine.backend.take_network_statuses(), [false]);
        app.power.set_conditions(Conditions::default());
        app.sync_power(&ctx);
        assert_eq!(app.engine.backend.take_network_statuses(), [true]);

        app.follows_reachability = false;
        app.power.set_conditions(offline);
        app.sync_power(&ctx);
        assert!(app.engine.backend.take_network_statuses().is_empty());
    }

    /// #789: one App Nap activity is held while local playback plays, and
    /// let go once it pauses.
    #[test]
    fn local_playback_holds_one_activity_while_it_plays() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.sync_power(&ctx);
        assert!(app.playback_activity.is_none());
        app.engine.local.connected = true;
        app.engine.local.playback = Playback::Playing;
        app.sync_power(&ctx);
        assert!(app.playback_activity.is_some());
        app.sync_power(&ctx);
        assert!(app.playback_activity.is_some(), "the same one, kept");
        app.engine.local.playback = Playback::Paused;
        app.sync_power(&ctx);
        assert!(app.playback_activity.is_none());
    }

    #[test]
    fn network_error_sanitization_never_exposes_raw_url() {
        use crate::api::ApiError;
        use crate::i18n::Locale;
        use crate::ui::widgets;

        let raw_network_err = ApiError::Network(
            "error sending request for url (https://api.spotify.com/v1/playlists/xyz): dns error"
                .into(),
        );

        let page_err = friendly_page_error(&crate::i18n::GettextLocalize, Locale::English, &raw_network_err);
        assert_eq!(page_err, "Network connection error. Check your connection.");
        assert!(!page_err.contains("https://"));
        assert!(!page_err.contains("api.spotify.com"));
        assert!(!page_err.contains("dns error"));

        let api_err = friendly_api_error(&crate::i18n::GettextLocalize, Locale::English, &raw_network_err);
        assert_eq!(api_err, "Network connection error. Check your connection.");
        assert!(!api_err.contains("https://"));
        assert!(!api_err.contains("api.spotify.com"));

        let sanitized = widgets::sanitize_error_message(
            "error sending request for url (https://api.spotify.com/v1/me): connection closed",
            Locale::English,
        );
        assert_eq!(
            sanitized.as_ref(),
            "Network connection error. Check your connection."
        );

        let server_err =
            widgets::sanitize_error_message("500 Internal Server Error", Locale::English);
        assert_eq!(server_err.as_ref(), "500 Internal Server Error");
    }
}
