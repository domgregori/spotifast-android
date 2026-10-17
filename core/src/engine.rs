//! The portable application engine: the state the views read and the actions
//! they ask for, minus the desktop's window, tray, and media wiring.
//!
//! `src/app.rs`'s `App` today mixes this state with the egui/fastframe shell.
//! The extraction moves the state and its transitions here, so both the
//! desktop and an Android front end drive the same engine. The one seam back
//! into the front end is [`UiContext`], for the window and clipboard effects a
//! UI action asks for: the desktop adapts `egui::Context`, Android adapts its
//! own surface (or no-ops where there is no equivalent).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::api::models::{Device, PlayableItem, PlaybackState, Playlist, Queue, Track, User};
use crate::locale::Locale;
use crate::api::PlayRequest;
use crate::backend::{AuthStatus, Backend, LocalPlayback};
use crate::model::{
    Action, AlbumPage, ArtistPage, Dialog, HomeData, Library, Loadable, Page, PlaylistPage,
    QueueTab, RadioPage, RowSelection, SearchState, ShowPage, TableRowsCache, TableSort, Toast, ToastKind,
};
use crate::now_playing::NowPlaying;
use crate::paths::AppDirs;
use crate::player::LocalState;
use crate::settings::Settings;

/// Translates the interface's strings, supplied by the front end. The desktop
/// uses gettext; Android resolves its own language or no-ops.
pub trait Localize: Send + Sync {
    fn t(&self, locale: Locale, source: &'static str) -> Cow<'static, str>;
    fn tn(&self, locale: Locale, singular: &'static str, plural: &'static str, count: u32) -> Cow<'static, str>;
}

pub trait UiContext: Send {
    fn fullscreen(&self, on: bool);
    fn maximized(&self, on: bool);
    fn minimized(&self, on: bool);
    fn focus(&self);
    fn close(&self);
    fn copy_text(&self, text: String);
    fn open_url(&self, url: String);
    fn include_image(&self, uri: String, bytes: Vec<u8>);
    fn repaint(&self);
    fn forget_images(&self);
    fn viewport_fullscreen(&self) -> bool;
    fn viewport_maximized(&self) -> bool;
}

pub struct NoUiContext;

impl UiContext for NoUiContext {
    fn fullscreen(&self, _on: bool) {}
    fn maximized(&self, _on: bool) {}
    fn minimized(&self, _on: bool) {}
    fn focus(&self) {}
    fn close(&self) {}
    fn copy_text(&self, _text: String) {}
    fn open_url(&self, _url: String) {}
    fn include_image(&self, _uri: String, _bytes: Vec<u8>) {}
    fn repaint(&self) {}
    fn forget_images(&self) {}
    fn viewport_fullscreen(&self) -> bool { false }
    fn viewport_maximized(&self) -> bool { false }
}

/// The state of another device's playback, as last polled.
pub struct RemoteSnapshot {
    pub state: PlaybackState,
    pub received_at: Instant,
}

/// A context shown as playing before Spotify confirms it.
pub struct AssumedContext {
    pub uri: String,
    pub shuffle: Option<bool>,
    pub at: Instant,
}

/// A track the interface shows before playback has reported its new state.
pub struct TrackIntent {
    pub uri: String,
    pub position_ms: u32,
    pub at: Instant,
    pub confirmation: TrackConfirmation,
}

/// What saving the edit dialog sends: only the details that changed.
pub struct PlaylistDetailChanges {
    pub name: Option<String>,
    pub description: Option<String>,
    pub public: Option<bool>,
    pub kept_description: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackConfirmation {
    Local,
    Remote { after_poll: u64, mismatches: u8 },
}

#[derive(Clone, Debug)]
pub struct QueueShufflePending {
    pub current_uri: Option<String>,
    pub context_uris: Vec<String>,
    pub at: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Local,
    Remote(Option<String>),
}

pub struct PendingAlbumQueue {
    pub id: String,
    pub label: String,
    pub target: Target,
    pub offset: u32,
    pub tracks: Vec<Track>,
}

pub struct PendingQueueAdd {
    pub item: PlayableItem,
    pub at: Instant,
    pub manual_index: usize,
    pub write: Option<(u64, usize)>,
}

pub struct PendingPaste {
    pub playlist_id: String,
    pub playlist_name: String,
    pub uris: Vec<String>,
    pub found: HashMap<String, PlayableItem>,
    pub missing: HashSet<String>,
}

impl PendingPaste {
    pub fn settled(&self) -> bool {
        self.uris.iter().all(|uri| self.found.contains_key(uri) || self.missing.contains(uri))
    }
}

/// Listening time for the current track.
pub struct Listening {
    pub uri: String,
    pub listened: std::time::Duration,
    pub playing_since: Option<Instant>,
    pub recorded: bool,
}

/// The portable application state: the data and intents the views read and the
/// actions mutate, without the window, tray, media, and update wiring.
pub struct Engine {
    pub localize: std::sync::Arc<dyn Localize>,
    pub locale: Locale,
    pub dirs: AppDirs,
    pub settings: Settings,
    /// Proxy policy actually handed to network workers. Manual form edits are
    /// drafts until Apply (or Sign in), so unrelated downloads and engine
    /// restarts must not observe them early.
    pub applied_proxy: crate::proxy::ProxyConfig,
    pub proxy_request: u64,
    pub last_proxy_applied: u64,
    pub proxy_form_edited: bool,
    pub settings_dirty: bool,
    pub last_settings_save: Instant,
    pub backend: Backend,
    /// Sample data is loaded; Spotify requests are disabled.
    pub offline: bool,
    /// OS reports network path is available.
    pub network_reachable: bool,
    pub auth: AuthStatus,
    pub user: Option<User>,
    pub local_device_id: Option<String>,
    /// Ignore the previous local song until a Connect transfer reports its track.
    pub local_transfer_sequence: Option<u64>,
    /// Local playback is authorized and the engine is connected.
    pub local_ready: bool,
    pub local_playback: LocalPlayback,
    pub local: LocalState,
    pub remote: Option<RemoteSnapshot>,
    pub remote_polled_at: Instant,
    pub remote_poll_pending: bool,
    /// When a poll last went out early for the end of a song on another
    /// device.
    pub remote_end_polled_at: Option<Instant>,
    /// When another device last accepted a command or a switch from here.
    pub remote_command_at: Option<Instant>,
    /// Serial of the newest playback poll sent; older answers are stale.
    pub remote_poll_seq: u64,
    /// The restorable session (sorts, recents, resume point) changed and
    /// should be written shortly, not only at exit.
    pub session_dirty: bool,
    pub last_session_save: Instant,
    pub devices: Vec<Device>,
    /// Receivers seen on the local network. Spotify lists a receiver only
    /// once it has an account, so these are the ones it cannot see yet.
    pub receivers: Vec<crate::zeroconf::Receiver>,
    /// The receiver currently being handed the account, by name.
    pub activating_receiver: Option<String>,
    /// Whether music was playing when the receiver was chosen. Activation
    /// can stop playback before the receiver is ready, so the transfer after
    /// it plays if music played then or plays now.
    pub activating_play: bool,
    pub devices_loading: bool,
    pub devices_fetched_at: Option<Instant>,
    pub selected_device: Option<String>,
    pub queue: Loadable<Queue>,
    pub queue_fetched_at: Option<Instant>,
    /// Latest queue request sequence. Older responses are discarded.
    pub queue_seq: u64,
    /// When to retry after a stale queue response.
    pub queue_recheck_at: Option<Instant>,
    pub queue_stale_retries: u8,
    /// Rows removed by Clear queue, used to detect stale responses.
    pub queue_cleared: Option<(std::collections::HashSet<String>, Instant)>,
    /// Upcoming context order before shuffle was toggled, used to detect lagging responses.
    pub queue_shuffle_pending: Option<QueueShufflePending>,
    /// When a local reorder or positional insert last landed, used to
    /// reject a fetch whose queued rows still show the pre-move order.
    pub queue_reorder_pending: Option<Instant>,
    /// Songs taken out of Playing next one by one, and when, used to
    /// reject a fetch that still has one right after the queued rows.
    pub queue_removed: Option<(std::collections::HashSet<String>, Instant)>,
    pub library: Library,
    pub liked_songs: crate::liked::LikedSongs,
    pub liked_recheck_at: Option<Instant>,
    pub home: HomeData,
    /// Local play history. See [`crate::history`].
    pub plays: crate::history::History,
    /// Current track timing used to decide when a play counts.
    pub listening: Option<Listening>,
    pub recents: crate::model::CursorList<crate::api::models::PlayHistory>,
    /// The Recent tab's rows: what was played here and what
    /// Spotify knows of the other devices, as one list. Rebuilt when
    /// either side changes rather than every frame.
    pub recents_view: Vec<crate::api::models::PlayHistory>,
    pub recents_generation: u64,
    pub queue_tab: QueueTab,
    pub search: SearchState,
    pub playlist_pages: HashMap<String, PlaylistPage>,
    /// One checkpoint snapshot at a time, including pages evicted while it writes.
    pub playlist_cache_write_in_flight: bool,
    pub load_generation: u64,
    pub album_pages: HashMap<String, AlbumPage>,
    pub artist_pages: HashMap<String, ArtistPage>,
    pub show_pages: HashMap<String, ShowPage>,
    /// Radio pages by the seed's URI.
    pub radio_pages: HashMap<String, RadioPage>,
    pub track_cache: HashMap<String, Track>,
    pub track_requests: HashSet<String>,
    /// Album URIs already resolved or attempted through librespot this session.
    pub album_types_requested: HashSet<String>,
    /// Saved shows Spotify marks as audiobooks, which librespot cannot play;
    /// the Podcasts shelf leaves them out.
    pub audiobook_shows: HashSet<String>,
    /// Saved shows already asked about.
    pub audiobooks_requested: HashSet<String>,
    /// Headers the streaming session read for library playlists the Web API
    /// listed blank, by playlist id, so a reloaded list fills at once.
    pub playlist_headers: HashMap<String, Playlist>,
    /// Blank library playlists already asked about.
    pub playlist_headers_requested: HashSet<String>,
    /// Album URIs positively identified as EPs by librespot.
    pub confirmed_ep_albums: HashSet<String>,
    /// Built table rows, keyed by page. Capped; dropped on reset and eviction.
    pub table_rows: HashMap<Page, TableRowsCache>,
    pub page_used: HashMap<Page, Instant>,
    pub track_used: HashMap<String, Instant>,
    pub history: Vec<Page>,
    pub history_index: usize,
    pub saved: HashMap<String, bool>,
    pub saved_pending: HashSet<String>,
    /// Spotify may expose one recording under several market-specific track
    /// URIs. Map those URIs to the recording identity returned by the API.
    pub track_recordings: HashMap<String, String>,
    /// `track_recordings` the other way round: each recording's URIs, so
    /// whether a recording is saved is answered from its own few URIs
    /// rather than by scanning every saved URI, once per track of a page.
    pub recording_uris: HashMap<String, HashSet<String>>,
    /// Known playlist-track availability for this account. Keep positive
    /// answers too, so an older disk cache cannot make a song unavailable
    /// again after the Web API has confirmed it can play.
    pub playlist_availability: HashMap<String, bool>,
    /// Recording identities for which at least one known URI is saved.
    pub saved_recordings: HashSet<String>,
    /// Optimistic library writes that a stale contains response must not undo.
    pub saved_writes: HashMap<String, bool>,
    pub accent_pending: HashSet<String>,
    pub dialog: Option<Dialog>,
    pub cover_request: u64,
    pub cover_uploads: HashMap<String, u64>,
    /// Successful uploads stay visible while Spotify propagates the new image.
    pub uploaded_covers: std::collections::HashMap<String, crate::playlist_cover::PendingCover>,
    pub show_queue_panel: bool,
    pub show_lyrics_panel: bool,
    pub lyrics_fullscreen: Option<bool>,
    pub lyrics_fullscreen_seen: bool,
    pub lyrics_fullscreen_restoring: Option<bool>,
    pub lyrics_restore_maximized: bool,
    /// The track the lyrics below are for.
    pub lyrics_uri: Option<String>,
    /// `Loaded(None)` when no lyrics are available.
    pub lyrics: Loadable<Option<crate::lyrics::Lyrics>>,
    /// Whether the panel follows the current line. Manual scrolling disables
    /// it until Follow is used or the track changes.
    pub lyrics_following: bool,
    /// The line the panel last positioned itself for (`Some(None)` before
    /// the first line), so it moves once per change; `None` until it has
    /// positioned itself at all for this track.
    pub lyrics_line_shown: Option<Option<usize>>,
    pub show_devices: bool,
    pub toasts: Vec<Toast>,
    pub actions: Vec<Action>,
    pub volume_before_mute: Option<u8>,
    /// Context and track URIs whose pending play buttons show a spinner.
    pub pending_play_keys: Vec<String>,
    pub pending_play_at: Option<Instant>,
    /// A play request made while the local engine was still connecting; it
    /// starts the moment the engine reports ready.
    pub queued_play: Option<PlayRequest>,
    /// Last list sent to local playback. Used for autoplay because librespot
    /// cannot continue a list without a context URI.
    pub local_list: Option<Vec<String>>,
    /// Activated receiver waiting to appear in Spotify's device list.
    pub pending_transfer_to: Option<(String, Instant)>,
    /// When to take a confirming look at remote playback after a command.
    pub remote_recheck_at: Option<Instant>,
    pub seek_preview: Option<f32>,
    pub volume_preview: Option<f32>,
    pub last_eviction: Instant,
    /// Playback snapshot for the current frame, built once per redraw.
    pub frame_now: Option<NowPlaying>,
    pub sign_in_url: Option<String>,
    /// The verified personal Web API application, when acceleration is ready.
    pub web_app: Option<String>,
    pub pending_remote_position: Option<(u32, Instant)>,
    pub pending_remote_volume: Option<(u8, Instant)>,
    /// A local volume set here that the engine has not echoed back yet. It
    /// reports `VolumeChanged` asynchronously while position snapshots land
    /// every second, so a snapshot must not undo the change on its way past.
    pub pending_local_volume: Option<(u16, Instant)>,
    /// A local seek position set here that the engine has not confirmed yet.
    pub pending_local_position: Option<(u32, Instant)>,
    pub optimistic_playing: Option<(bool, Instant)>,
    /// Track shown immediately after a play or skip, until playback reports.
    pub intent_track: Option<TrackIntent>,
    /// Requested shuffle mode, applied to every context until changed.
    pub shuffle_wanted: bool,
    /// Last local shuffle change, used to ignore its echo from the engine.
    pub shuffle_set_at: Option<Instant>,
    /// When tracks recently came up unavailable, to spot a key-service
    /// cascade and reconnect once instead of skipping through an album.
    pub unavailable_at: Vec<Instant>,
    pub last_unavailable_reconnect: Option<Instant>,
    /// The Premium notice has been shown for this sign-in.
    pub premium_notice_shown: bool,
    /// Context shown immediately after play, until Spotify confirms it. An
    /// empty URI means a plain track list, whose lack of a context must also
    /// hide stale state.
    pub assumed_context: Option<AssumedContext>,
    pub last_now_playing_uri: Option<String>,
    pub last_now_playing_sequence: u64,
    /// A requested start whose effect on the manual queue is already applied.
    /// Context loads keep it; explicit skips consume their rows immediately.
    pub queue_start_pending: Option<Target>,
    pub playlist_busy: bool,
    pub quit_requested: bool,
    /// How each table is sorted, per page, for as long as the app runs.
    /// The rows picked out in a track table, and the page they belong to.
    /// One table at a time: picking rows on another page replaces it.
    /// The page it belongs to, what that page's list looked like when the
    /// rows were picked, and the rows.
    pub selection: Option<(Page, String, RowSelection)>,
    pub table_sorts: HashMap<Page, TableSort>,
    /// User ids resolved to display names; `None` while unknown, so an id
    /// is asked about only once per run.
    pub user_names: HashMap<String, Option<String>>,
    pub user_names_revision: u64,
    /// User ids resolved to avatar picture urls, asked for alongside the
    /// names; `None` while unknown or when Spotify gives no picture.
    pub user_images: HashMap<String, Option<String>>,
    /// Context URIs most recently played, newest first: the sidebar's
    /// order. Kept with the session, so it survives a restart.
    pub recent_contexts: Vec<String>,
    /// What was playing when the app last closed, to resume from cold.
    pub resume_context: Option<String>,
    pub resume_track: Option<String>,
    pub resume_position_ms: u32,
    /// Manually queued songs restored with the remembered track.
    pub resume_queue: Vec<String>,
    /// Manually queued songs from this session, oldest first.
    pub manual_queue: Vec<String>,
    /// Queue additions shown before Spotify confirms them, with request time.
    pub pending_queue_adds: Vec<PendingQueueAdd>,
    pub pending_album_queues: HashMap<u64, PendingAlbumQueue>,
    pub pending_queue_batches: HashMap<u64, Target>,
    pub album_queue_serial: u64,
    /// The latest `play-search` command. Its answer plays only while this
    /// still names it: a newer search or anything the user started since
    /// takes precedence over a search that finishes late.
    pub play_search_serial: u64,
    pub last_album_queue: Option<(String, Instant)>,
    /// The account's playlist tree from Spotify, folders and all; empty
    /// until the session answers.
    pub rootlist: Vec<crate::player::RootlistEntry>,
    /// Last good tree and the account it belongs to, kept across restarts.
    pub rootlist_cache: Option<crate::settings::CachedRootlist>,
    /// The read of the verified account's cached library on its way.
    pub account_caches_generation: u64,
    /// A session connected and its account's cache is still to be read,
    /// once `Me` says which account it is.
    pub account_caches_due: bool,
    /// When the tree was last asked for.
    pub rootlist_read_at: Option<Instant>,
    /// Whether the window had focus last frame.
    pub window_focused: bool,
    /// Coming back to the window asked for the tree, and it has not been
    /// read since.
    pub rootlist_wanted: bool,
    /// Playlists the account may add songs to by Spotify's own word, by
    /// URI: the ones shared with it by invitation, which the Web API's
    /// collaborative flag does not show. Empty until the session answers.
    pub editable_by_grant: std::collections::BTreeSet<String>,
    /// A Spotify link handed over from outside, waiting for the account
    /// to be signed in and, for a song, for its album to be known.
    pub pending_link: Option<String>,
    pub prerelease_request: Option<u64>,
    pub prerelease_serial: u64,
    /// The songs behind the links last copied from a track table, so a
    /// paste of those links adds rows with their names straight away.
    pub copied_songs: Vec<PlayableItem>,
    /// Pasted song links waiting for Spotify to describe the songs this
    /// app has not seen, before they are added to their playlist.
    pub pending_pastes: Vec<PendingPaste>,
    /// Sidebar folders rolled up, by their rootlist ids.
    pub collapsed_folders: Vec<String>,
}

impl Engine {
    pub fn page(&self) -> &Page {
        &self.history[self.history_index]
    }

    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty = true;
    }

    pub fn can_go_back(&self) -> bool {
        self.history_index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.history_index + 1 < self.history.len()
    }

    pub fn toast(&mut self, message: impl Into<String>) {
        self.toasts.push(Toast {
            message: message.into(),
            kind: ToastKind::Info,
            created: Instant::now(),
        });
        self.toasts.truncate(4);
    }

    pub fn toast_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        log::warn!("{message}");
        self.toasts.push(Toast {
            message,
            kind: ToastKind::Error,
            created: Instant::now(),
        });
    }
}

