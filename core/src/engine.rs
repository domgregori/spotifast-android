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

use crate::api::models::{Album, Artist, Device, PlayableItem, PlaybackState, Playlist, PlaylistItem, Queue, Show, Track, TrackCount, User, UserRef, pick_image};
use crate::locale::Locale;
use crate::api::PlayRequest;
use crate::backend::{AuthStatus, Backend, LocalPlayback, LyricsRequest};
use crate::model::{
    Action, AlbumPage, ArtistPage, Dialog, DiscographyFilter, HomeData,
    HomeSnapshot, Library, Loadable, Page, PagedList, PlayingFrom, PlaylistCache, PlaylistEdit,
    PlaylistCachePending, PlaylistPage, QueueTab, RadioPage, RowContext, RowPick, RowSelection, SearchState, ShowPage,
    TableRowsCache, TableSort, Toast, ToastKind,
};
use crate::now_playing::NowPlaying;
use crate::paths::AppDirs;
use crate::settings::{CachedRootlist, Settings};
use std::time::Duration;

use crate::backend::{ApiRequest, ApiResponse, Command, Event, PLAYLIST_PAGE_SIZE, PlaylistCacheRows, RecentsFor, RemoteAction};
use crate::player::{LoadSpec, LocalState, Playback, PlayerCommand, RepeatMode};
use crate::util;
use crate::util::{percent_to_volume, volume_to_percent};

/// Translates the interface's strings, supplied by the front end. The desktop
/// uses gettext; Android resolves its own language or no-ops.
pub trait Localize: Send + Sync {
    fn t(&self, locale: Locale, source: &'static str) -> Cow<'static, str>;
    fn tn(&self, locale: Locale, singular: &'static str, plural: &'static str, count: u32) -> Cow<'static, str>;
}

/// Off-thread file writes, supplied by the front end. The desktop hands
/// writes to a `fastframe_file::Writer`; the Android front end its own writer.
pub trait FileWriter: Send + Sync {
    fn write(&self, path: &std::path::Path, bytes: Vec<u8>);
    fn write_private(&self, path: &std::path::Path, bytes: Vec<u8>);
    fn flush(&self, timeout: Duration) -> bool;
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

pub const REMOTE_POLL_ACTIVE: Duration = Duration::from_secs(4);
pub const REMOTE_POLL_IDLE: Duration = Duration::from_secs(20);
pub const REMOTE_POLL_MIN: Duration = Duration::from_secs(15);
pub const REMOTE_COMMAND_WINDOW: Duration = Duration::from_secs(10);
pub const REMOTE_POLL_UNSEEN: Duration = Duration::from_secs(30);
pub const REMOTE_FRESH: Duration = Duration::from_secs(60);
pub const DEVICES_FRESH: Duration = Duration::from_secs(12);
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(280);
pub const SUSPEND_FLUSH_TIMEOUT: Duration = Duration::from_millis(1500);
pub const SESSION_REFRESH: Duration = Duration::from_secs(30);
pub const HEADLESS_SLEEP: Duration = Duration::from_secs(2);
pub const PLAYLISTS_RETRY: Duration = Duration::from_secs(60);
pub const BACKGROUND_QUIET: Duration = Duration::from_secs(1);
pub const ROOTLIST_REFRESH: Duration = Duration::from_secs(5);
pub const RESTART_BEFORE_PREVIOUS: u32 = 3_000;
pub const TOAST_LIFETIME: Duration = Duration::from_millis(3200);
pub const TOAST_FRAME: Duration = Duration::from_millis(33);
pub const OPTIMISTIC_HOLD: Duration = Duration::from_millis(2500);
pub const ASSUMED_CONTEXT_HOLD: Duration = Duration::from_secs(8);
pub const PLAYBACK_HOLD: Duration = Duration::from_secs(6);
pub const REMOTE_RECHECK: Duration = Duration::from_millis(1200);
pub const QUEUE_RECHECK: Duration = Duration::from_millis(700);
pub const QUEUE_STALE_RETRIES: u8 = 6;
pub const QUEUE_ADD_DEBOUNCE: Duration = Duration::from_millis(1500);
pub const RECENT_CONTEXTS_KEPT: usize = 60;
pub const CONTAINS_BATCH: usize = 40;
pub const HOME_RECENTS: u32 = 50;
pub const HOME_PODCAST_SHOWS: usize = 8;
pub const RECENTS_PAGE: u32 = 50;

/// The portable application state: the data and intents the views read and the
/// actions mutate, without the window, tray, media, and update wiring.
pub struct Engine {
    pub localize: std::sync::Arc<dyn Localize>,
    pub files: std::sync::Arc<dyn FileWriter>,
    pub accents: HashMap<String, [u8; 3]>,
    pub control_devices_stale: bool,
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

impl Engine {


    /// Watches the queue control clients fill and keeps the snapshots they
    /// read -- now playing, and the device list -- fresh.


    /// Per-window setup: fonts, icons, loaders, theme. Called every time a
    /// window is (re)created around this long-lived application state.


    /// The window is gone but the process stays: audio, the tray, and the
    /// media controls keep running until Show or Quit.


    /// Whether closing the window keeps the app in the tray rather than
    /// quitting.


    // ---- derived state -----------------------------------------------------

    pub fn is_connected(&self) -> bool {
        matches!(self.auth, AuthStatus::Connected { .. })
    }

    pub fn user_id(&self) -> Option<&str> {
        self.user.as_ref().map(|user| user.id.as_str())
    }

    /// The library list's entry for a playlist, when it holds one.
    /// Fills library playlists the Web API listed blank from the headers
    /// the streaming session read, and asks it once about the rest.
    pub fn fill_blank_playlists(&mut self) {
        let Some(playlists) = self.library.playlists.get_mut() else {
            return;
        };
        let mut unread = Vec::new();
        for playlist in playlists
            .iter_mut()
            .filter(|playlist| playlist.name.is_empty() && !playlist.id.is_empty())
        {
            match self.playlist_headers.get(&playlist.id) {
                Some(header) => playlist.fill_blank_from(header),
                None => {
                    if self.playlist_headers_requested.insert(playlist.id.clone()) {
                        unread.push(playlist.id.clone());
                    }
                }
            }
        }
        if !unread.is_empty() {
            self.backend.send(Command::PlaylistHeaders(unread));
        }
    }

    pub fn library_entry(&self, id: &str) -> Option<&Playlist> {
        self.library
            .playlists
            .get()?
            .iter()
            .find(|playlist| playlist.id == id)
    }

    /// The signed-in account's display name, when `owner` is that account.
    pub fn own_name(&self, owner: Option<&str>) -> Option<String> {
        self.user
            .as_ref()
            .filter(|user| Some(user.id.as_str()) == owner)?
            .display_name
            .clone()
    }

    pub fn is_saved(&self, uri: &str) -> Option<bool> {
        let exact = self.saved.get(uri).copied();
        if exact == Some(true)
            || self.track_recordings
                .get(uri)
                .is_some_and(|key| self.saved_recordings.contains(key))
        {
            Some(true)
        } else {
            exact
        }
    }

    /// Album, Single, EP, Compilation or Appears On, in the interface language.
    pub fn album_kind_label(&self, album: &Album) -> Cow<'static, str> {
        if album.is_single_release() && self.confirmed_ep_albums.contains(&album.uri) {
            Cow::Borrowed("EP")
        } else {
            match album
                .album_group
                .as_deref()
                .or(album.album_type.as_deref())
                .unwrap_or("album")
            {
                "single" => self.localize.t(self.locale, "Single"),
                "compilation" => self.localize.t(self.locale, "Compilation"),
                "appears_on" => self.localize.t(self.locale, "Appears On"),
                _ => self.localize.t(self.locale, "Album"),
            }
        }
    }

    /// Whether any URI of the recording is saved.
    pub fn recording_saved(&self, key: &str) -> bool {
        self.recording_uris
            .get(key)
            .is_some_and(|uris| uris.iter().any(|uri| self.saved.get(uri) == Some(&true)))
    }

    pub fn remember_track_recording(&mut self, track: &Track) {
        let Some(key) = track.recording_key() else {
            return;
        };
        link_recording(
            &mut self.track_recordings,
            &mut self.recording_uris,
            track.uri.clone(),
            key.clone(),
        );
        if let Some(linked) = &track.linked_from
            && !linked.uri.is_empty()
        {
            link_recording(
                &mut self.track_recordings,
                &mut self.recording_uris,
                linked.uri.clone(),
                key.clone(),
            );
        }
        if self.recording_saved(&key) {
            self.saved_recordings.insert(key);
        }
    }

    pub fn set_saved_state(&mut self, uri: String, saved: bool) {
        self.saved.insert(uri.clone(), saved);
        let Some(key) = self.track_recordings.get(&uri).cloned() else {
            return;
        };
        if saved {
            self.saved_recordings.insert(key);
        } else if !self.recording_saved(&key) {
            self.saved_recordings.remove(&key);
        }
    }

    pub fn saved_toggle_targets(&self, uri: &str) -> Vec<String> {
        let Some(key) = self.track_recordings.get(uri) else {
            return vec![uri.to_string()];
        };
        let equivalents: Vec<String> = self.recording_uris
            .get(key)
            .into_iter()
            .flatten()
            .filter(|candidate| self.saved.get(*candidate) == Some(&true))
            .cloned()
            .collect();
        if equivalents.is_empty() {
            vec![uri.to_string()]
        } else {
            equivalents
        }
    }

    pub fn remote_fresh(&self) -> Option<&RemoteSnapshot> {
        self.remote
            .as_ref()
            .filter(|remote| remote.received_at.elapsed() < REMOTE_FRESH)
    }

    /// Where playback commands go: this computer's player or a remote device.
    pub fn target(&self) -> Target {
        if self.local_ready && self.local.is_active() {
            return Target::Local;
        }
        if let Some(selected) = &self.selected_device
            && Some(selected.as_str()) != self.local_device_id.as_deref()
        {
            return Target::Remote(Some(selected.clone()));
        }
        if let Some(remote) = self.remote_fresh() {
            let device = remote.state.device.as_ref();
            let is_local = device
                .and_then(|device| device.id.as_deref())
                .is_some_and(|id| Some(id) == self.local_device_id.as_deref());
            if !is_local && (remote.state.is_playing || remote.state.item.is_some()) {
                return Target::Remote(device.and_then(|device| device.id.clone()));
            }
        }
        if self.local_ready {
            Target::Local
        } else {
            Target::Remote(None)
        }
    }

    /// Context shown as playing, including pending local play requests.
    pub fn playing_context_uri(&self) -> Option<String> {
        let remote = self.remote
            .as_ref()
            .and_then(|remote| remote.state.context.as_ref())
            .map(|context| crate::link::canonical_context_uri(&context.uri));
        if let Some(assumed) = &self.assumed_context {
            let held = assumed.at.elapsed() < ASSUMED_CONTEXT_HOLD;
            // A filtered or sorted context plays as plain tracks and will not
            // report a context URI. Keep the assumed URI unless contradicted
            // by a poll taken since the request: one from before it still
            // tells the story from before, whatever device it describes.
            let contradicted = self.remote.as_ref().is_some_and(|snapshot| {
                snapshot.received_at > assumed.at
                    && snapshot.state.context.as_ref().is_some_and(|context| {
                        crate::link::canonical_context_uri(&context.uri) != assumed.uri
                    })
            });
            if held || (!contradicted && self.believed_playing()) {
                return (!assumed.uri.is_empty()).then(|| assumed.uri.clone());
            }
        }
        remote
    }

    /// Track shown as current, including a recent unconfirmed play request.
    pub fn current_track_uri(&self) -> Option<String> {
        if let Some(intent) = &self.intent_track
            && (intent.at.elapsed() < PLAYBACK_HOLD || self.queued_play.is_some())
        {
            return Some(intent.uri.clone());
        }
        self.now_playing().map(|now| now.uri)
    }

    /// Shows a user-requested track until the responsible playback surface
    /// has had a chance to report what really started.
    pub fn expect_track(&mut self, uri: String, position_ms: u32) {
        let confirmation = match self.target() {
            Target::Local => TrackConfirmation::Local,
            Target::Remote(_) => TrackConfirmation::Remote {
                after_poll: self.remote_poll_seq,
                mismatches: 0,
            },
        };
        self.intent_track = Some(TrackIntent {
            uri,
            position_ms,
            at: Instant::now(),
            confirmation,
        });
    }

    /// The local engine is the playback authority. A real track change wins
    /// even when the queue's optimistic guess named a different track.
    pub fn reconcile_local_track_intent(&mut self, track_changed: bool, reported: Option<&str>) {
        let settled = self.intent_track.as_ref().is_some_and(|intent| {
            matches!(intent.confirmation, TrackConfirmation::Local)
                && (track_changed || reported == Some(intent.uri.as_str()))
        });
        if settled {
            self.intent_track = None;
        }
    }

    /// A poll issued before the command cannot settle its intent. Spotify's
    /// first later answer may still describe the old track, so a mismatch is
    /// held and checked once more. Agreement, or two later mismatches, settles
    /// on the reported state.
    pub fn reconcile_remote_track_intent(&mut self, poll: u64, reported: Option<&str>) {
        let mut settled = false;
        let mut recheck = false;
        if let Some(intent) = &mut self.intent_track
            && let TrackConfirmation::Remote {
                after_poll,
                mismatches,
            } = &mut intent.confirmation
            && poll > *after_poll
        {
            if reported == Some(intent.uri.as_str()) || *mismatches > 0 {
                settled = true;
            } else {
                *after_poll = poll;
                *mismatches += 1;
                recheck = true;
            }
        }
        if settled {
            self.intent_track = None;
        } else if recheck {
            self.remote_recheck_at = Some(Instant::now() + REMOTE_RECHECK);
        }
    }

    /// Playback state shown by the UI, including a recent local request.
    pub fn believed_playing(&self) -> bool {
        if let Some((playing, at)) = self.optimistic_playing
            && at.elapsed() < PLAYBACK_HOLD
        {
            return playing;
        }
        self.now_playing().is_some_and(|now| now.playing)
    }

    pub fn playing_context_shuffle(&self) -> bool {
        self.shuffle_wanted
    }

    /// Current item for menus, using cached track details when available.
    pub fn now_playing_item(&self) -> Option<PlayableItem> {
        let now = self.now_playing()?;
        if now.is_episode {
            return None;
        }
        if let Some(track) = now.id.as_deref().and_then(|id| self.track_cache.get(id)) {
            return Some(PlayableItem::Track(track.clone()));
        }
        Some(PlayableItem::Track(Track {
            id: now.id.clone(),
            uri: now.uri.clone(),
            name: now.title.clone(),
            artists: now.artists.clone(),
            duration_ms: now.duration_ms,
            ..Track::default()
        }))
    }

    pub fn now_playing(&self) -> Option<NowPlaying> {
        if let Some(now) = &self.frame_now {
            return Some(now.clone());
        }
        self.requested_track_preview()
            .or_else(|| self.now_playing_live())
            .or_else(|| self.resume_preview())
    }



    /// Keep the player bar on the same requested song as the playlist marker
    /// while the responsible playback surface catches up.
    pub fn requested_track_preview(&self) -> Option<NowPlaying> {
        let intent = self.intent_track.as_ref()?;
        if intent.at.elapsed() >= PLAYBACK_HOLD && self.queued_play.is_none() {
            return None;
        }
        let mut now = self.cached_track_preview(&intent.uri, intent.position_ms)?;
        let actual = self.now_playing_live();
        now.local = matches!(intent.confirmation, TrackConfirmation::Local);
        now.playing = match self.optimistic_playing {
            Some((playing, at)) if at.elapsed() < PLAYBACK_HOLD => playing,
            _ => self.queued_play.is_some() || actual.as_ref().is_some_and(|now| now.playing),
        };
        now.loading = true;
        now.resuming = false;
        if let Some(actual) = actual {
            now.device_name = actual.device_name;
            now.device_kind = actual.device_kind;
            now.repeat = actual.repeat;
            now.volume_percent = actual.volume_percent;
            now.can_control = actual.can_control;
            now.can_set_volume = actual.can_set_volume;
        }
        Some(now)
    }

    /// What a device is actually playing, here or elsewhere.
    pub fn now_playing_live(&self) -> Option<NowPlaying> {
        if self.local.is_active() && self.local_transfer_sequence != Some(self.local.track_sequence)
        {
            let track = self.local.track.as_ref()?;
            let cached = track
                .uri
                .rsplit(':')
                .next()
                .and_then(|id| self.track_cache.get(id));
            // Playback already carries artist IDs. Keep those links available
            // while the Web API is loading (or its cached credits have no IDs).
            let artists = if track.artists.iter().any(|artist| artist.id.is_some()) {
                track.artists.clone()
            } else {
                cached
                    .map(|cached| cached.artists.clone())
                    .unwrap_or_else(|| track.artists.clone())
            };
            let playing = match self.optimistic_playing {
                Some((playing, at)) if at.elapsed() < PLAYBACK_HOLD => playing,
                _ => self.local.playback == Playback::Playing,
            };
            return Some(NowPlaying {
                local: true,
                device_name: None,
                device_kind: None,
                uri: track.uri.clone(),
                id: util::uri_id(&track.uri).map(str::to_string),
                title: track.title.clone(),
                subtitle: track.artist_names(),
                artists,
                album_name: track.album.clone(),
                album_id: cached
                    .and_then(|cached| cached.album.as_ref())
                    .map(|album| album.id.clone()),
                show_id: None,
                art_url: track.art_url.clone(),
                art_small: track
                    .art_small_url
                    .clone()
                    .or_else(|| track.art_url.clone()),
                duration_ms: track.duration_ms,
                position_ms: match self.pending_local_position {
                    Some((position, at)) if at.elapsed() < OPTIMISTIC_HOLD => {
                        if self.local.playback == Playback::Playing {
                            let elapsed = at.elapsed().as_millis() as u32;
                            let limit = track.duration_ms.max(position);
                            position.saturating_add(elapsed).min(limit)
                        } else {
                            position
                        }
                    }
                    _ => self.local.position_now(),
                },
                playing,
                // `Playback` keeps reading `Playing` through a load so the
                // controls stay visible; the engine's own loading flag is
                // what the spinner follows.
                loading: self.local.loading,
                shuffle: self.shuffle_wanted,
                repeat: self.local.repeat,
                volume_percent: volume_to_percent(self.local.volume),
                can_control: true,
                can_set_volume: true,
                is_episode: track.is_episode,
                resuming: false,
            });
        }
        let remote = self.remote_fresh()?;
        // Ignore a remote snapshot for this device when the local engine has
        // no track. The snapshot may predate a local stop.
        if self.local_device_id.is_some()
            && remote
                .state
                .device
                .as_ref()
                .and_then(|device| device.id.as_deref())
                == self.local_device_id.as_deref()
        {
            return None;
        }
        let item = remote.state.item.as_ref()?;
        let device = remote.state.device.as_ref();
        let playing = match self.optimistic_playing {
            Some((playing, at)) if at.elapsed() < PLAYBACK_HOLD => playing,
            _ => remote.state.is_playing,
        };
        let position = match self.pending_remote_position {
            Some((position, at)) if at.elapsed() < OPTIMISTIC_HOLD => position,
            _ => {
                let base = remote.state.progress_ms.unwrap_or(0);
                if remote.state.is_playing {
                    (base as u64 + remote.received_at.elapsed().as_millis() as u64)
                        .min(item.duration_ms() as u64) as u32
                } else {
                    base
                }
            }
        };
        let volume = match self.pending_remote_volume {
            Some((volume, at)) if at.elapsed() < OPTIMISTIC_HOLD => volume,
            _ => device
                .and_then(|device| device.volume_percent)
                .unwrap_or(50),
        };
        let (artists, album_name, album_id, show_id, is_episode) = match item {
            PlayableItem::Track(track) => (
                track.artists.clone(),
                track
                    .album
                    .as_ref()
                    .map(|album| album.name.clone())
                    .unwrap_or_default(),
                track.album.as_ref().map(|album| album.id.clone()),
                None,
                false,
            ),
            PlayableItem::Episode(episode) => (
                Vec::new(),
                episode
                    .show
                    .as_ref()
                    .map(|show| show.name.clone())
                    .unwrap_or_default(),
                None,
                episode.show.as_ref().map(|show| show.id.clone()),
                true,
            ),
        };
        Some(NowPlaying {
            local: false,
            device_name: device.map(|device| device.name.clone()),
            device_kind: device.map(|device| device.kind.clone()),
            uri: item.uri().to_string(),
            id: item.id().map(str::to_string),
            title: item.name().to_string(),
            subtitle: item.subtitle(),
            artists,
            album_name,
            album_id,
            show_id,
            art_url: item.image(640).map(str::to_string),
            art_small: item.image(64).map(str::to_string),
            duration_ms: item.duration_ms(),
            position_ms: position,
            playing,
            loading: false,
            shuffle: self.shuffle_wanted,
            repeat: RepeatMode::from_api(&remote.state.repeat_state),
            volume_percent: volume,
            // A restricted device is driven over the playback session's
            // Connect link instead, see `crate::sonos`.
            can_control: device.is_none_or(|device| !device.is_restricted || self.local_ready),
            can_set_volume: device.is_none_or(|device| {
                (!device.is_restricted || self.local_ready) && device.supports_volume != Some(false)
            }),
            is_episode,
            resuming: false,
        })
    }

    /// Last session's track, shown paused when no device is playing.
    pub fn resume_preview(&self) -> Option<NowPlaying> {
        let uri = self.resume_track.as_deref()?;
        self.cached_track_preview(uri, self.resume_position_ms)
    }

    pub fn cached_track_preview(&self, uri: &str, position_ms: u32) -> Option<NowPlaying> {
        let track = self.track_cache.get(util::uri_id(uri)?)?;
        Some(NowPlaying {
            local: true,
            device_name: None,
            device_kind: None,
            uri: uri.to_string(),
            id: track.id.clone(),
            title: track.name.clone(),
            subtitle: track.artist_names(),
            artists: track.artists.clone(),
            album_name: track
                .album
                .as_ref()
                .map(|album| album.name.clone())
                .unwrap_or_default(),
            album_id: track.album.as_ref().map(|album| album.id.clone()),
            show_id: None,
            art_url: track.image(640).map(str::to_string),
            art_small: track.image(64).map(str::to_string),
            duration_ms: track.duration_ms,
            position_ms: position_ms.min(track.duration_ms),
            playing: false,
            loading: false,
            shuffle: self.shuffle_wanted,
            repeat: RepeatMode::Off,
            volume_percent: volume_to_percent(self.local.volume),
            can_control: true,
            can_set_volume: true,
            is_episode: false,
            resuming: true,
        })
    }

    /// The play request for `key` (a context or track URI) is still waiting
    /// for Spotify to react.
    pub fn play_pending(&self, key: &str) -> bool {
        self.pending_fresh() && self.pending_play_keys.iter().any(|k| k == key)
    }

    /// Records who is behind a user id. A lookup that found nothing never
    /// replaces a name already known: when the session connects, names
    /// still being looked up are asked for again, and the lookup that
    /// failed may answer after the one that found the name.
    pub fn set_user_name(&mut self, id: String, name: Option<String>) {
        if name.is_none() && self.user_names.get(&id).is_some_and(Option::is_some) {
            return;
        }
        if self.user_names.get(&id) != Some(&name) {
            self.user_names.insert(id, name);
            self.user_names_revision = self.user_names_revision.wrapping_add(1);
        }
    }

    /// Records the picture behind a user id, under the same rule as the
    /// name: a lookup that found nothing never replaces one already known.
    pub fn set_user_image(&mut self, id: String, image: Option<String>) {
        if image.is_none() && self.user_images.get(&id).is_some_and(Option::is_some) {
            return;
        }
        if self.user_images.get(&id) != Some(&image) {
            self.user_images.insert(id, image);
        }
    }

    pub fn any_play_pending(&self) -> bool {
        self.pending_fresh() && !self.pending_play_keys.is_empty()
    }

    pub fn pending_fresh(&self) -> bool {
        // A request queued behind a connecting engine stays pending for as
        // long as the engine may take; an ordinary request times out fast.
        self.queued_play.is_some()
            || self.pending_play_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(8))
    }

    pub fn set_play_pending(&mut self, keys: Vec<String>) {
        self.pending_play_keys = keys;
        self.pending_play_at = Some(Instant::now());
    }

    pub fn clear_play_pending(&mut self) {
        self.pending_play_keys.clear();
        self.pending_play_at = None;
    }

    /// The artwork width to ask Spotify for, for art drawn `points` wide on
    /// the window's display.


    /// The playing art for a cover drawn `points` wide: the small rendition
    /// while it is sharp enough, the large one once the display needs more
    /// pixels than the small one has.


    /// The colour to tint the interface with, from the playing art.




    /// Finds a playlist already loaded in the library, search, or Home shelf.
    pub fn known_playlist(&self, id: &str) -> Option<&Playlist> {
        self.library
            .playlists
            .get()
            .and_then(|playlists| playlists.iter().find(|playlist| playlist.id == id))
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.playlists.as_ref())
                    .and_then(|playlists| playlists.items.iter().find(|playlist| playlist.id == id))
            })
            .or_else(|| {
                self.home
                    .made_for_you
                    .get()
                    .and_then(|playlists| playlists.iter().find(|playlist| playlist.id == id))
            })
    }

    pub fn known_album(&self, id: &str) -> Option<&Album> {
        self.library
            .albums
            .items
            .iter()
            .find(|saved| saved.album.id == id)
            .map(|saved| &saved.album)
            .or_else(|| {
                let results = self.search.results.get()?;
                results
                    .albums
                    .as_ref()
                    .and_then(|albums| albums.items.iter().find(|album| album.id == id))
                    .or_else(|| {
                        results.tracks.as_ref().and_then(|tracks| {
                            tracks
                                .items
                                .iter()
                                .filter_map(|track| track.album.as_ref())
                                .find(|album| album.id == id)
                        })
                    })
            })
            .or_else(|| {
                self.library
                    .liked
                    .items
                    .iter()
                    .filter_map(|saved| saved.track.album.as_ref())
                    .find(|album| album.id == id)
            })
            .or_else(|| {
                self.playlist_pages.values().find_map(|page| {
                    page.items
                        .items
                        .iter()
                        .find_map(|item| match item.playable() {
                            Some(PlayableItem::Track(track)) => {
                                track.album.as_ref().filter(|album| album.id == id)
                            }
                            _ => None,
                        })
                })
            })
            .or_else(|| {
                self.artist_pages.values().find_map(|page| {
                    page.albums
                        .values()
                        .find_map(|albums| albums.items.iter().find(|album| album.id == id))
                })
            })
            .or_else(|| {
                [
                    &self.home.top_tracks,
                    &self.home.top_songs,
                    &self.home.recommendations,
                ]
                .into_iter()
                .find_map(|tracks| {
                    tracks.get().and_then(|tracks| {
                        tracks
                            .iter()
                            .filter_map(|track| track.album.as_ref())
                            .find(|album| album.id == id)
                    })
                })
            })
    }

    pub fn known_artist(&self, id: &str) -> Option<&Artist> {
        self.library
            .artists
            .items
            .iter()
            .find(|artist| artist.id == id)
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.artists.as_ref())
                    .and_then(|artists| artists.items.iter().find(|artist| artist.id == id))
            })
            .or_else(|| {
                self.home
                    .top_artists
                    .get()
                    .and_then(|artists| artists.iter().find(|artist| artist.id == id))
            })
            .or_else(|| {
                self.artist_pages.values().find_map(|page| {
                    page.related
                        .get()
                        .and_then(|artists| artists.iter().find(|artist| artist.id == id))
                })
            })
    }

    pub fn known_show(&self, id: &str) -> Option<&Show> {
        self.library
            .shows
            .items
            .iter()
            .find(|saved| saved.show.id == id)
            .map(|saved| &saved.show)
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.shows.as_ref())
                    .and_then(|shows| shows.items.iter().find(|show| show.id == id))
            })
            .or_else(|| {
                self.library
                    .episodes
                    .items
                    .iter()
                    .filter_map(|saved| saved.episode.show.as_ref())
                    .find(|show| show.id == id)
            })
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.episodes.as_ref())
                    .and_then(|episodes| {
                        episodes
                            .items
                            .iter()
                            .filter_map(|episode| episode.show.as_ref())
                            .find(|show| show.id == id)
                    })
            })
    }

    // ---- frame ---------------------------------------------------------------





    pub fn handle_auth(&mut self, status: AuthStatus) {
        match &status {
            AuthStatus::Connected { .. } => {
                self.sign_in_url = None;
                self.reset_data();
                // `Me` follows and reads the cache, even for the same
                // account signing in again.
                self.account_caches_due = true;
                self.load_playlists();
                self.ensure_loaded(self.page().clone());
                self.poll_remote(true);
            }
            AuthStatus::WaitingForBrowser { url } => self.sign_in_url = Some(url.clone()),
            AuthStatus::SignedOut => {
                if matches!(self.dialog, Some(Dialog::PersonalAppIntro)) {
                    self.dialog = None;
                }
                self.uploaded_covers.clear();
                self.sign_in_url = None;
                self.web_app = None;
                self.user = None;
                self.premium_notice_shown = false;
                self.local = LocalState::default();
                self.local_ready = false;
                self.local_device_id = None;
                self.local_playback = LocalPlayback::Unavailable;
                self.remote = None;
                self.rootlist.clear();
                self.rootlist_cache = None;
                self.editable_by_grant.clear();
                self.account_caches_due = false;
                self.session_dirty = true;
                self.reset_data();
            }
            AuthStatus::Failed(message) => {
                self.sign_in_url = None;
                self.toast_error(message.clone());
            }
            _ => {}
        }
        self.auth = status;
    }

    /// Retries the Home shelf when local playback becomes ready after Home.
    pub fn handle_playback(&mut self, status: LocalPlayback) {
        match &status {
            LocalPlayback::Ready { device_id } => {
                self.local_device_id = Some(device_id.clone());
                let became_ready = !self.local_ready;
                self.local_ready = true;
                if became_ready
                    && self.home.requested
                    && (!self.home.made_for_you_personalized
                        || !self.home.made_for_you_session_requested)
                {
                    self.load_made_for_you(false);
                }
                self.replay_queued_play();
            }
            LocalPlayback::Unavailable => {
                self.local_ready = false;
                self.local_device_id = None;
            }
            LocalPlayback::Failed(message) => {
                self.local_ready = false;
                if self.queued_play.take().is_some() {
                    self.clear_play_pending();
                    self.intent_track = None;
                }
                self.toast_error(
                    // Translators: {error} is an error message from the playback engine.
                    self.localize.t(self.locale, "Local playback: {error}").replace("{error}", message),
                );
            }
            LocalPlayback::Authorizing => {
                if !matches!(self.local_playback, LocalPlayback::Authorizing) {
                    self.toast(self.localize.t(
                        self.locale,
                        "Opening your browser to set up local playback",
                    ));
                }
            }
            LocalPlayback::Connecting | LocalPlayback::WaitingForNetwork => {}
        }
        self.local_playback = status;
    }

    pub fn on_network_restored(&mut self) {
        if !self.is_connected() {
            return;
        }
        if self.library.playlists.needs_load()
            || self.library.playlists_retry_at.is_some()
            || self.library.playlists_loading_since.is_some()
        {
            self.load_playlists();
        }
        let page = self.page().clone();
        match page {
            Page::Home => {
                // A failed answer may have left cached shelves showing.
                let incomplete = self.home.recently_played.get().is_none()
                    || self.home.top_artists.get().is_none()
                    || self.home.top_tracks.get().is_none()
                    || self.home.refresh_failed;
                if incomplete {
                    self.load_home(true);
                }
            }
            Page::TopSongs => {
                if self.home.top_songs.get().is_none() {
                    self.load_top_songs(true);
                }
            }
            Page::Search => {}
            Page::LikedSongs => {
                if !self.liked_songs.cache_checked {
                    self.ensure_liked_songs();
                } else if !self.library.liked.loaded_once || self.library.liked.error.is_some() {
                    self.library.liked.error = None;
                    self.load_more(Page::LikedSongs);
                }
            }
            Page::Albums => {
                if !self.library.albums.loaded_once || self.library.albums.error.is_some() {
                    self.library.albums.error = None;
                    self.load_more(Page::Albums);
                }
            }
            Page::Artists => {
                if !self.library.artists.loaded_once || self.library.artists.error.is_some() {
                    self.library.artists.error = None;
                    self.load_more(Page::Artists);
                }
            }
            Page::Podcasts => {
                if !self.library.shows.loaded_once || self.library.shows.error.is_some() {
                    self.library.shows.error = None;
                    self.load_more(Page::Podcasts);
                }
            }
            Page::Episodes => {
                if !self.library.episodes.loaded_once || self.library.episodes.error.is_some() {
                    self.library.episodes.error = None;
                    self.load_more(Page::Episodes);
                }
            }
            Page::Playlist(id) => {
                let incomplete = match self.playlist_pages.get(&id) {
                    None => true,
                    Some(page) => {
                        page.playlist.needs_load()
                            || !page.items.loaded_once
                            || page.items.error.is_some()
                    }
                };
                if incomplete {
                    if let Some(page) = self.playlist_pages.get_mut(&id) {
                        page.items.error = None;
                    }
                    self.ensure_loaded(Page::Playlist(id));
                }
            }
            Page::Album(id) => {
                let incomplete = match self.album_pages.get(&id) {
                    None => true,
                    Some(page) => {
                        page.album.needs_load()
                            || !page.tracks.loaded_once
                            || page.tracks.error.is_some()
                    }
                };
                if incomplete {
                    if let Some(page) = self.album_pages.get_mut(&id) {
                        page.tracks.error = None;
                    }
                    self.ensure_loaded(Page::Album(id));
                }
            }
            Page::Artist(id) => {
                let incomplete = match self.artist_pages.get(&id) {
                    None => true,
                    Some(page) => {
                        let filter = page.filter;
                        let albums_need = page
                            .albums
                            .get(filter.groups())
                            .is_none_or(|list| !list.loaded_once || list.error.is_some());
                        page.artist.needs_load() || albums_need || page.related.needs_load()
                    }
                };
                if incomplete {
                    if let Some(page) = self.artist_pages.get_mut(&id) {
                        let filter = page.filter;
                        if let Some(list) = page.albums.get_mut(filter.groups()) {
                            list.error = None;
                        }
                    }
                    self.ensure_loaded(Page::Artist(id));
                }
            }
            Page::Show(id) => {
                let incomplete = match self.show_pages.get(&id) {
                    None => true,
                    Some(page) => {
                        page.show.needs_load()
                            || !page.episodes.loaded_once
                            || page.episodes.error.is_some()
                    }
                };
                if incomplete {
                    if let Some(page) = self.show_pages.get_mut(&id) {
                        page.episodes.error = None;
                    }
                    self.ensure_loaded(Page::Show(id));
                }
            }
            Page::Radio(seed) => {
                let incomplete = self.radio_pages
                    .get(&seed)
                    .is_none_or(|page| page.songs.needs_load());
                if incomplete {
                    self.ensure_loaded(Page::Radio(seed));
                }
            }
            Page::Queue => {
                if self.queue.needs_load() {
                    self.refresh_queue(true);
                }
            }
            Page::Settings => {}
        }
    }

    /// Clears account data while preserving request serials that reject late answers.


    /// Drop table-row caches whose pages are gone, and cap what remains.
    pub fn retain_table_rows(&mut self, current: &Page) {
        const MAX_TABLE_ROW_CACHES: usize = 2;
        self.table_rows.retain(|page, _| {
            page == current
                || match page {
                    Page::Playlist(id) => self.playlist_pages.contains_key(id),
                    Page::Album(id) => self.album_pages.contains_key(id),
                    Page::LikedSongs | Page::TopSongs => true,
                    _ => false,
                }
        });
        if self.table_rows.len() <= MAX_TABLE_ROW_CACHES {
            return;
        }
        let mut keep = HashSet::from([current.clone()]);
        if let Some(page) = self.history.get(self.history_index) {
            keep.insert(page.clone());
        }
        if self.history_index > 0
            && let Some(page) = self.history.get(self.history_index - 1)
        {
            keep.insert(page.clone());
        }
        self.table_rows.retain(|page, _| keep.contains(page));
        while self.table_rows.len() > MAX_TABLE_ROW_CACHES {
            let drop = self.table_rows
                .keys()
                .find(|page| *page != current)
                .cloned();
            match drop {
                Some(page) => {
                    self.table_rows.remove(&page);
                }
                None => break,
            }
        }
    }

    pub fn table_rows_retained_bytes(&self) -> usize {
        self.table_rows
            .values()
            .map(TableRowsCache::retained_bytes)
            .sum()
    }



    /// Loads details for the remembered track before playback starts.
    pub fn request_resume_track(&mut self) {
        if self.now_playing_live().is_some() {
            return;
        }
        let Some(uri) = self.resume_track.clone() else {
            return;
        };
        // Episodes are not in the track endpoint; the preview skips them.
        if !uri.starts_with("spotify:track:") {
            return;
        }
        let Some(id) = util::uri_id(&uri).map(str::to_string) else {
            return;
        };
        if self.track_cache.contains_key(&id) {
            self.track_used.insert(id, Instant::now());
            return;
        }
        if !self.track_requests.insert(id.clone()) {
            return;
        }
        self.backend.api(ApiRequest::Track { id });
    }

    /// Whether the player bar shows the remembered track without live playback.
    pub fn resume_only(&self) -> bool {
        self.resume_track.is_some() && self.now_playing_live().is_none()
    }

    /// Loads the remembered context so Previous and Next work before playback.
    pub fn ensure_resume_context_loaded(&mut self) {
        if !self.resume_only() {
            return;
        }
        let Some(context) = self.resume_context.clone() else {
            return;
        };
        if self.context_track_uris(&context).is_some() {
            return;
        }
        if let Some(page) = Page::decode(&Self::context_page(&context)) {
            self.ensure_loaded(page);
        }
    }

    /// Where the playing songs come from, for the queue's header: the
    /// playlist, album, artist, or podcast with its page, Liked Songs, or
    /// a song radio as plain text because a station has no page. A context
    /// whose name has not loaded is still named by kind so it can open.
    pub fn playing_from(&self) -> Option<PlayingFrom> {
        let context = self.playing_context_uri()?;
        if context.ends_with(":collection") {
            return Some(PlayingFrom {
                name: self.localize.t(self.locale, "Liked Songs").into_owned(),
                page: Some(Page::LikedSongs),
            });
        }
        if context.starts_with("spotify:station:") {
            return Some(PlayingFrom {
                name: self
                    .station_name(&context)
                    .unwrap_or_else(|| self.localize.t(self.locale, "Radio").into_owned()),
                page: util::station_seed(&context).map(Page::Radio),
            });
        }
        let kind = util::uri_kind(&context)?;
        let id = util::uri_id(&context)?.to_string();
        let (name, page) = match kind {
            "playlist" => {
                let name = self
                    .library_entry(&id)
                    .map(|playlist| playlist.name.clone())
                    .or_else(|| {
                        self.playlist_pages
                            .get(&id)
                            .and_then(|page| page.playlist.get())
                            .map(|playlist| playlist.name.clone())
                    });
                (
                    name.unwrap_or_else(|| self.localize.t(self.locale, "Playlist").into_owned()),
                    Page::Playlist(id),
                )
            }
            "album" => {
                let name = self.album_pages
                    .get(&id)
                    .and_then(|page| page.album.get())
                    .map(|album| album.name.clone())
                    .or_else(|| {
                        self.now_playing()
                            .filter(|now| now.album_id.as_deref() == Some(id.as_str()))
                            .map(|now| now.album_name)
                    });
                (
                    name.unwrap_or_else(|| self.localize.t(self.locale, "Album").into_owned()),
                    Page::Album(id),
                )
            }
            "artist" => {
                let name = self.artist_pages
                    .get(&id)
                    .and_then(|page| page.artist.get())
                    .map(|artist| artist.name.clone())
                    .or_else(|| {
                        self.now_playing().and_then(|now| {
                            now.artists
                                .into_iter()
                                .find(|artist| artist.id.as_deref() == Some(id.as_str()))
                                .map(|artist| artist.name)
                        })
                    });
                (
                    name.unwrap_or_else(|| self.localize.t(self.locale, "Artist").into_owned()),
                    Page::Artist(id),
                )
            }
            "show" => {
                let name = self.show_pages
                    .get(&id)
                    .and_then(|page| page.show.get())
                    .map(|show| show.name.clone())
                    .or_else(|| {
                        self.library
                            .shows
                            .items
                            .iter()
                            .find(|saved| saved.show.id == id)
                            .map(|saved| saved.show.name.clone())
                    });
                (
                    name.unwrap_or_else(|| self.localize.t(self.locale, "Podcast").into_owned()),
                    Page::Show(id),
                )
            }
            _ => return None,
        };
        Some(PlayingFrom {
            name,
            page: Some(page),
        })
    }

    /// "<Name> Radio" for a station whose seed's name is known.


    /// Encodes a context as a value accepted by `Page::decode`.
    pub fn context_page(context_uri: &str) -> String {
        if context_uri.ends_with(":collection") {
            return "liked".to_owned();
        }
        match (util::uri_kind(context_uri), util::uri_id(context_uri)) {
            (Some(kind), Some(id)) => format!("{kind}:{id}"),
            _ => String::new(),
        }
    }

    /// Moves the paused remembered track within its context without playing.
    /// Returns `false` when the context is unavailable.
    pub fn step_resume(&mut self, forward: bool) -> bool {
        let Some(context) = self.resume_context.clone() else {
            return false;
        };
        let Some(uris) = self.context_track_uris(&context) else {
            return false;
        };
        let current = self.resume_track.clone().unwrap_or_default();
        let next = if self.shuffle_wanted && forward {
            // Preserve shuffle behavior before playback resumes.
            let choices: Vec<&String> = uris.iter().filter(|uri| **uri != current).collect();
            if choices.is_empty() {
                return false;
            }
            choices[rand::random_range(0..choices.len())].clone()
        } else {
            let Some(index) = uris.iter().position(|uri| *uri == current) else {
                return false;
            };
            let last = uris.len() - 1;
            let target = match (forward, index) {
                (true, i) if i == last => 0,
                (true, i) => i + 1,
                (false, 0) => last,
                (false, i) => i - 1,
            };
            uris[target].clone()
        };
        self.cache_track_from_context(&context, &next);
        self.resume_track = Some(next);
        self.resume_position_ms = 0;
        self.session_dirty = true;
        true
    }

    /// Caches track details from a loaded context for immediate display.
    pub fn cache_track_from_context(&mut self, context_uri: &str, uri: &str) {
        let Some(id) = util::uri_id(uri) else {
            return;
        };
        if self.track_cache.contains_key(id) {
            self.track_used.insert(id.to_owned(), Instant::now());
            return;
        }
        let found = if let Some(pid) = context_uri.strip_prefix("spotify:playlist:") {
            self.playlist_pages.get(pid).and_then(|page| {
                page.items
                    .items
                    .iter()
                    .find_map(|item| match item.playable() {
                        Some(PlayableItem::Track(track)) if track.uri == uri => Some(track.clone()),
                        _ => None,
                    })
            })
        } else if let Some(aid) = context_uri.strip_prefix("spotify:album:") {
            self.album_pages.get(aid).and_then(|page| {
                page.tracks
                    .items
                    .iter()
                    .find(|track| track.uri == uri)
                    .cloned()
            })
        } else if context_uri.ends_with(":collection") {
            self.library
                .liked
                .items
                .iter()
                .find(|item| item.track.uri == uri)
                .map(|item| item.track.clone())
        } else {
            None
        };
        if let Some(track) = found {
            self.remember_track_recording(&track);
            self.track_cache.insert(id.to_owned(), track);
            self.track_used.insert(id.to_owned(), Instant::now());
        }
    }

    /// Older playlist caches predate recording identities. Once the current
    /// track supplies an ISRC, refresh only plausible aliases so Spotify can
    /// confirm which market-specific URI is the same recording.
    pub fn request_recording_candidates(&mut self, track: &Track) {
        if track.recording_key().is_none() {
            return;
        }
        let mut candidates = HashSet::new();
        for page in self.playlist_pages.values() {
            for item in &page.items.items {
                let Some(PlayableItem::Track(candidate)) = item.playable() else {
                    continue;
                };
                if same_recording_hint(track, candidate)
                    && let Some(id) = candidate.id.as_ref()
                    && self.track_cache
                        .get(id)
                        .is_none_or(|known| known.recording_key().is_none())
                {
                    candidates.insert(id.clone());
                }
            }
        }
        for id in candidates {
            if self.track_requests.insert(id.clone()) {
                self.backend.api(ApiRequest::Track { id });
            }
        }
    }



    /// Asks for the playing track's lyrics unless they are here or on the
    /// way. Podcasts have no lyrics to ask for.


    /// Pushes the Winamp window's always-on-top level to the live window.




    /// Loads and applies the skin selected in settings.
    /// On failure, restores the active skin setting to avoid repeated retries.


    /// Syncs MilkDrop settings and receives window state and commands.

    /// How many frames a second the player bar's and mini player's
    /// visualizers draw now, from the user's rate and the power budget;
    /// `None` draws none.


    /// The screen rate the visualizers' frame-rate dial offers: the one
    /// showing the main window, or the one MilkDrop last saw until that
    /// has said; 0 while neither is known.


    /// Records the MilkDrop screen refresh rate and uses it as the initial FPS.
    /// Later screen changes do not override a configured FPS, and an answer
    /// that is no rate, such as Windows's "hardware default", is ignored.

    /// Note that the restorable session changed, so it is saved shortly.
    pub fn note_session_change(&mut self) {
        self.session_dirty = true;
    }

    pub fn handle_proxy_restored(
        &mut self,
        config: crate::proxy::ProxyConfig,
        password: Option<crate::credentials::ProxyPassword>,
    ) {
        if self.last_proxy_applied == 0 {
            self.applied_proxy = config;
        }
        if !self.proxy_form_edited
            && self.proxy_request == 0
            && let Some(password) = password
        {
            self.settings.restore_proxy_password(&password);
        }
    }









    /// Called at launch or by the local reload command. Construction and window
    /// attachment never scan theme files.


    /// Adds the desktop's palettes (Omarchy on Linux) for a normal launch.




    /// Takes the selected and the desktop's palettes from a finished scan.










    /// The Dock menu's playback items, read with or without a window.
    #[cfg(target_os = "macos")]
    pub fn handle_dock_menu(&mut self) {
        use crate::mac_menu::MenuCommand;
        for command in crate::mac_menu::drain_dock_commands() {
            match command {
                MenuCommand::PlayPause => self.actions.push(Action::TogglePlay),
                MenuCommand::Next => self.actions.push(Action::Next),
                MenuCommand::Previous => self.actions.push(Action::Previous),
                _ => {}
            }
        }
    }

    /// The MacBook notch widget items, read with or without a window.
    #[cfg(target_os = "macos")]
    pub fn handle_notch_commands(&mut self) {
        for command in crate::notch::drain_commands() {
            self.actions.push(command.action());
        }
    }





    /// The downloaded file for `url`, once the art cache has it.
    ///
    /// Windows and macOS are handed a file rather than the URL, so the disk
    /// is asked until the download lands and the answer remembered after
    /// that; macOS aborts on a remote image that fails to load. The
    /// player bar only ever draws the small cover, so on a miss the full-size
    /// artwork is fetched here -- the one request the controls add.
    ///
    /// MPRIS passes `art_url` to the desktop, which fetches whatever it
    /// wants: Linux needs no file and downloads nothing extra.






    /// Whether to offer hiding the mini player's taskbar entry.




    /// One line for the control channel's `nowplaying` verb: tab-separated
    /// `state, title, artists, album, position_ms, duration_ms, volume,
    /// shuffle, repeat, art_url, saved, device`, or
    /// [`crate::single_instance::NOTHING_PLAYING`].
    ///
    /// The last three are what a Stream Deck key needs and a media key does
    /// not: something to draw, whether the heart is filled, and where the
    /// sound is coming out. They are appended rather than woven in, so a
    /// script written against the older nine fields still reads correctly.


    /// Last Spotify Connect device list as one JSON line.
    ///
    /// JSON safely carries multiple records and free-text device names.


    // ---- loading ---------------------------------------------------------------

    pub fn read_rootlist(&mut self) {
        self.rootlist_wanted = false;
        self.rootlist_read_at = Some(Instant::now());
        self.backend.send(Command::Rootlist);
    }

    /// Spotify's own apps can reorder playlists and folders while Spotifast
    /// waits behind them, so coming back reads the tree again. The read
    /// goes over the playback session and spends no Web API quota. A return
    /// soon after the last read waits out the rest of `ROOTLIST_REFRESH`,
    /// and the time left is returned so the caller can wake up for it.
    ///
    /// `focused` is `None` on a frame that does not say, such as one run
    /// without a window; that frame neither leaves nor comes back.


    /// Reads the playlists from the top. A list already shown stays until
    /// the new one is whole. A load already on its way is replaced, not
    /// joined: whatever asked again did so after something changed.
    pub fn load_playlists(&mut self) {
        // Nothing is asked for offline: the list loads once the network is
        // back.
        if !self.network_reachable {
            self.library.playlists_retry_at = Some(Instant::now());
            return;
        }
        let library = &mut self.library;
        if library.playlists.get().is_some() {
            library.playlists_incoming = Some(Vec::new());
        } else {
            library.playlists = Loadable::Loading;
            library.playlists_incoming = None;
            library.playlists_whole = false;
        }
        library.playlists_next = None;
        library.playlists_asked = None;
        library.playlists_retry_at = None;
        library.playlists_loading_since = Some(Instant::now());
        library.playlists_generation += 1;
        self.backend.api(ApiRequest::MyPlaylists {
            offset: 0,
            generation: library.playlists_generation,
        });
    }

    /// Records a playlist created or removed here, replacing an earlier
    /// change to the same playlist.
    pub fn edit_playlists(&mut self, edit: PlaylistEdit) {
        let edits = &mut self.library.playlists_edits;
        edits.retain(|earlier| earlier.uri != edit.uri);
        edits.push(edit);
        self.apply_playlist_edits();
    }

    /// Marks a removal as confirmed by Spotify: only a load asked for from
    /// now on can show it. A failed one is forgotten, so the next list
    /// shows the playlist again.
    pub fn settle_playlist_edit(&mut self, uri: &str, confirmed: bool) {
        let settled_by = self.library.playlists_generation + 1;
        let edits = &mut self.library.playlists_edits;
        if confirmed {
            if let Some(edit) = edits.iter_mut().find(|edit| edit.uri == uri) {
                edit.settled_by = Some(settled_by);
            }
        } else {
            edits.retain(|edit| edit.uri != uri);
        }
    }

    pub fn apply_playlist_edits(&mut self) {
        let library = &mut self.library;
        let Some(playlists) = library.playlists.get_mut() else {
            return;
        };
        for edit in &library.playlists_edits {
            match &edit.added {
                Some(playlist) => {
                    if !playlists.iter().any(|listed| listed.uri == edit.uri) {
                        playlists.insert(0, playlist.clone());
                    }
                }
                None => playlists.retain(|listed| listed.uri != edit.uri),
            }
        }
    }

    /// Takes what a whole list says about each listed playlist.
    pub fn note_listed_playlists(&mut self) {
        let Some(playlists) = self.library.playlists.get() else {
            return;
        };
        for listed in playlists {
            self.saved.insert(listed.uri.clone(), true);
            // A header read over the streaming session lacks what the list
            // carries; pages that arrived before the list take it now.
            if let Some(playlist) = self.playlist_pages
                .get_mut(&listed.id)
                .and_then(|page| page.playlist.get_mut())
            {
                playlist.fill_from(listed);
            }
        }
    }

    /// Saves the shown list for the next start, once it holds nothing
    /// Spotify has yet to confirm.
    pub fn store_library_cache(&mut self) {
        if self.library.playlists_loading_since.is_some()
            || !self.library.playlists_whole
            || self.library
                .playlists_edits
                .iter()
                .any(|edit| edit.settled_by.is_none())
        {
            return;
        }
        if let (Some(account), Some(playlists)) = (self.user_id(), self.library.playlists.get()) {
            self.backend.send(Command::StoreLibraryCache(
                crate::account_cache::Cache::new(account.to_owned(), playlists.clone()),
            ));
        }
    }

    /// Asks for the verified account's cached library, replacing any read
    /// still on its way.
    pub fn request_account_caches(&mut self) {
        self.load_generation = self.load_generation.wrapping_add(1);
        self.account_caches_generation = self.load_generation;
        self.backend.send(Command::LoadAccountCaches {
            generation: self.account_caches_generation,
        });
    }

    /// Shows the verified account's last playlist list and Home shelves
    /// while they are read again. What Spotify already answered with wins.
    pub fn receive_account_caches(
        &mut self,
        account: &str,
        generation: u64,
        playlists: Option<Vec<Playlist>>,
        home: Option<HomeSnapshot>,
    ) {
        if self.user_id() != Some(account) || generation != self.account_caches_generation {
            return;
        }
        if let Some(cached) = home {
            self.restore_home(cached);
        }
        if let Some(mut playlists) = playlists
            && self.library.playlists.get().is_none()
        {
            for playlist in &mut playlists {
                self.reconcile_playlist_cover(&playlist.id, &mut playlist.images);
            }
            self.library.playlists = Loadable::Loaded(playlists);
            self.library.playlists_whole = true;
            self.fill_blank_playlists();
            if self.library.playlists_loading_since.is_some() {
                // The load on its way, still before its first page, now
                // refreshes the restored list.
                self.library.playlists_incoming = Some(Vec::new());
            }
            self.apply_playlist_edits();
            self.note_listed_playlists();
        }
    }

    /// When each kind of background work under way began: refreshing the
    /// library, then checking for updates.


    /// What the top bar's spinner shows, once background work has gone on
    /// for a while. Refreshing the library comes first, as the work people
    /// wait on; other requests to Spotify come last.


    /// Fills the Home shelves Spotify has not answered for yet.
    pub fn restore_home(&mut self, cached: HomeSnapshot) {
        let home = &mut self.home;
        if let Some(recent) = cached.recently_played
            && home.recently_played.get().is_none()
        {
            home.recently_played = Loadable::Loaded(recent);
        }
        if let Some(artists) = cached.top_artists
            && home.top_artists.get().is_none()
        {
            home.top_artists = Loadable::Loaded(artists);
        }
        if let Some(tracks) = cached.top_tracks
            && home.top_tracks.get().is_none()
        {
            home.top_tracks = Loadable::Loaded(tracks);
        }
        if home.made_for_you.get().is_none() {
            let personalized = cached.made_for_you.is_some() && cached.made_for_you_personalized;
            let playlists = cached.made_for_you.or_else(|| {
                (!cached.discover.is_empty()).then(|| {
                    ["Discover Weekly", "Release Radar", "Daily Mix", "daylist"]
                        .iter()
                        .filter_map(|term| cached.discover.get(*term))
                        .flatten()
                        .cloned()
                        .collect()
                })
            });
            if let Some(mut playlists) = playlists {
                playlists
                    .retain(|playlist| playlist.uri != crate::made_for_you::UNSUPPORTED_DJ_URI);
                home.made_for_you = Loadable::Loaded(playlists);
                home.made_for_you_personalized = personalized;
            }
        }
        if !home.podcasts_answered && home.podcasts.is_empty() {
            home.podcasts = cached.podcasts;
        }
    }

    /// Counts one answer of the Home refresh, and saves the shelves when
    /// it brought anything new. Saving each one, rather than the whole
    /// refresh, keeps what arrived even when the shared app never answers.
    pub fn home_answered(&mut self, changed: bool) {
        self.home.awaiting = self.home.awaiting.saturating_sub(1);
        if changed {
            self.store_home_cache();
        }
    }

    /// Saves the visible shelves and their provenance for the verified account.
    pub fn store_home_cache(&mut self) {
        let Some(account) = self.user_id().map(str::to_owned) else {
            return;
        };
        let home = &self.home;
        let snapshot = HomeSnapshot {
            recently_played: home.recently_played.get().cloned(),
            top_artists: home.top_artists.get().cloned(),
            top_tracks: home.top_tracks.get().cloned(),
            made_for_you: home.made_for_you.get().cloned(),
            made_for_you_personalized: home.made_for_you_personalized,
            discover: Default::default(),
            podcasts: home.podcasts.clone(),
        };
        self.backend
            .send(Command::StoreHomeCache(crate::account_cache::Cache::new(
                account, snapshot,
            )));
    }



    pub fn load_artist_albums(&mut self, id: &str, filter: DiscographyFilter) {
        let Some(page) = self.artist_pages.get_mut(id) else {
            return;
        };
        let list = page.albums.entry(filter.groups().to_string()).or_default();
        if !list.loaded_once && list.can_load_more() {
            list.loading = true;
            self.backend.api(ApiRequest::ArtistAlbums {
                id: id.to_string(),
                groups: filter.groups().to_string(),
                offset: 0,
            });
        }
    }

    /// Starts a new Home refresh while preserving cards already on screen.
    pub fn load_home(&mut self, force: bool) {
        if self.home.requested
            && !force
            && self.home
                .loaded_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(600))
        {
            return;
        }
        self.home.requested = true;
        self.home.loaded_at = Some(Instant::now());
        self.home.generation += 1;
        // Recently played, top artists and top tracks. The shelf adds its
        // own pending answer; podcasts follow saved shows without a wait.
        self.home.awaiting = 3;
        self.home.made_for_you_loading = false;
        self.home.refresh_failed = false;
        let generation = self.home.generation;
        if self.home.recently_played.get().is_none() {
            self.home.recently_played = Loadable::Loading;
        }
        if self.home.top_artists.get().is_none() {
            self.home.top_artists = Loadable::Loading;
        }
        if self.home.top_tracks.get().is_none() {
            self.home.top_tracks = Loadable::Loading;
        }
        self.backend.api(ApiRequest::RecentlyPlayed {
            who: RecentsFor::Home,
            generation,
            before: None,
            limit: HOME_RECENTS,
        });
        self.backend.api(ApiRequest::TopArtists { generation });
        self.backend.api(ApiRequest::TopTracks {
            offset: 0,
            full: false,
            generation,
        });
        self.load_made_for_you(force);
        // The podcast shelf reads from the saved shows. The first page of
        // them is the one the Podcasts shelf of the library asks for.
        if self.library.shows.loaded_once {
            self.request_home_episodes();
        } else if !self.library.shows.loading && self.library.shows.error.is_none() {
            self.load_more(Page::Podcasts);
        }
    }

    /// Supersedes an older shelf request without clearing its visible cards.
    pub fn load_made_for_you(&mut self, force: bool) {
        self.home.made_for_you_generation += 1;
        if !self.home.made_for_you_loading {
            self.home.awaiting += 1;
        }
        self.home.made_for_you_loading = true;
        self.home.made_for_you_session_requested = self.local_ready;
        if self.home.made_for_you.get().is_none() {
            self.home.made_for_you = Loadable::Loading;
        }
        self.backend.api(ApiRequest::MadeForYou {
            generation: self.home.made_for_you_generation,
            force,
        });
    }

    /// Asks for the newest episodes of the most recently saved podcasts,
    /// once per Home refresh. Known audiobooks are skipped: librespot cannot
    /// play them.
    pub fn request_home_episodes(&mut self) {
        if self.home.podcasts_generation == self.home.generation {
            return;
        }
        self.home.podcasts_generation = self.home.generation;
        let shows: Vec<Show> = self.library
            .shows
            .items
            .iter()
            .map(|saved| &saved.show)
            .filter(|show| !show.id.is_empty() && !self.audiobook_shows.contains(&show.uri))
            .take(HOME_PODCAST_SHOWS)
            .cloned()
            .collect();
        if shows.is_empty() {
            self.home.podcasts.clear();
            self.home.podcasts_answered = true;
            return;
        }
        self.backend.api(ApiRequest::HomeEpisodes {
            shows,
            generation: self.home.generation,
        });
    }

    pub fn load_top_songs(&mut self, force: bool) {
        if self.home.top_songs_loading || (!force && self.home.top_songs_complete) {
            return;
        }
        self.home.top_songs = Loadable::Loading;
        self.home.top_songs_loading = true;
        self.home.top_songs_complete = false;
        self.home.top_songs_generation += 1;
        self.backend.api(ApiRequest::TopTracks {
            offset: 0,
            full: true,
            generation: self.home.top_songs_generation,
        });
    }

    pub fn load_recents(&mut self, force: bool) {
        if !self.network_reachable || self.recents.loading {
            return;
        }
        if self.recents.complete && !force {
            return;
        }
        if force {
            self.recents.reset();
        }
        self.recents.loading = true;
        self.recents.error = None;
        self.recents_generation = self.recents_generation.wrapping_add(1);
        let generation = self.recents_generation;
        // This endpoint paginates backwards, so its continuation cursor is
        // named `before`.
        let before = self.recents.after.clone();
        self.backend.api(ApiRequest::RecentlyPlayed {
            who: RecentsFor::Panel,
            generation,
            before,
            limit: RECENTS_PAGE,
        });
    }

    pub fn load_more_recents(&mut self) {
        if !self.recents.can_load_more() {
            return;
        }
        self.load_recents(false);
    }

    pub fn reload_recents(&mut self) {
        self.load_recents(true);
    }

    pub fn load_more(&mut self, page: Page) {
        if !self.network_reachable {
            if matches!(page, Page::LikedSongs) && !self.liked_songs.cache_checked {
                self.ensure_liked_songs();
            }
            return;
        }
        match page {
            Page::LikedSongs => {
                if !self.liked_songs.cache_checked {
                    self.ensure_liked_songs();
                } else if let Some(offset) = self.liked_songs.request_offset() {
                    self.backend.api(ApiRequest::SavedTracks {
                        offset,
                        generation: self.liked_songs.generation,
                    });
                    self.library.liked.loading = true;
                }
            }
            Page::Albums => {
                let list = &mut self.library.albums;
                if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                    list.loading = true;
                    self.backend.api(ApiRequest::SavedAlbums { offset });
                }
            }
            Page::Artists => {
                let list = &mut self.library.artists;
                if list.can_load_more() {
                    list.loading = true;
                    self.backend.api(ApiRequest::FollowedArtists {
                        after: list.after.clone(),
                    });
                }
            }
            Page::Podcasts => {
                let list = &mut self.library.shows;
                if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                    list.loading = true;
                    self.backend.api(ApiRequest::SavedShows { offset });
                }
            }
            Page::Episodes => {
                let list = &mut self.library.episodes;
                if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                    list.loading = true;
                    self.backend.api(ApiRequest::SavedEpisodes { offset });
                }
            }
            Page::Playlist(id) => {
                if self.playlist_pages.get(&id).is_some_and(|page| {
                    page.pending_writes > 0
                        || page.optimistic_snapshot.is_some()
                        || page.items.error.is_some()
                }) {
                    return;
                }
                let restart_from_top = self.playlist_pages.get(&id).is_some_and(|page| {
                    page.items.base_offset > 0
                        && (!page.filter.trim().is_empty()
                            || self.table_sorts.contains_key(&Page::Playlist(id.clone())))
                });
                if restart_from_top {
                    self.load_playlist_items_at(&id, 0);
                    return;
                }
                if let Some(page) = self.playlist_pages.get_mut(&id) {
                    let list = &mut page.items;
                    if let Some(offset) = list
                        .window_request
                        .or(list.next_offset)
                        .filter(|_| list.can_load_more())
                    {
                        list.loading = true;
                        self.backend.api(ApiRequest::PlaylistItems {
                            id,
                            offset,
                            generation: page.generation,
                        });
                    }
                }
            }
            Page::Album(id) => {
                if let Some(page) = self.album_pages.get_mut(&id) {
                    let list = &mut page.tracks;
                    if list.base_offset > 0
                        && self.table_sorts.contains_key(&Page::Album(id.clone()))
                    {
                        self.load_generation = self.load_generation.wrapping_add(1);
                        page.generation = self.load_generation;
                        let total = list.total;
                        list.reset();
                        list.total = total;
                    }
                    if let Some(offset) = list
                        .window_request
                        .or(list.next_offset)
                        .filter(|_| list.can_load_more())
                    {
                        list.loading = true;
                        self.backend.api(ApiRequest::AlbumTracks {
                            id,
                            offset,
                            generation: page.generation,
                        });
                    }
                }
            }
            Page::Show(id) => {
                if let Some(page) = self.show_pages.get_mut(&id) {
                    let list = &mut page.episodes;
                    if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                        list.loading = true;
                        self.backend.api(ApiRequest::ShowEpisodes { id, offset });
                    }
                }
            }
            Page::Home => {
                if let Some(offset) = self.library.playlists_next.take() {
                    self.library.playlists_asked = Some(offset);
                    self.backend.api(ApiRequest::MyPlaylists {
                        offset,
                        generation: self.library.playlists_generation,
                    });
                }
            }
            _ => {}
        }
    }

    pub fn load_window(&mut self, page: Page, position: u32) {
        match page {
            Page::Playlist(id) => {
                let Some(page) = self.playlist_pages.get_mut(&id) else {
                    return;
                };
                if page.pending_writes > 0
                    || page.optimistic_snapshot.is_some()
                    || !page.filter.trim().is_empty()
                    || self.table_sorts.contains_key(&Page::Playlist(id.clone()))
                {
                    return;
                }
                let offset = page.items.window_at(position, PLAYLIST_PAGE_SIZE);
                if let Some(offset) = offset {
                    self.backend.api(ApiRequest::PlaylistItems {
                        id,
                        offset,
                        generation: page.generation,
                    });
                }
            }
            Page::Album(id) => {
                let Some(page) = self.album_pages.get_mut(&id) else {
                    return;
                };
                if self.table_sorts.contains_key(&Page::Album(id.clone())) {
                    return;
                }
                if let Some(offset) = page.tracks.window_at(position, 50) {
                    self.backend.api(ApiRequest::AlbumTracks {
                        id,
                        offset,
                        generation: page.generation,
                    });
                }
            }
            _ => {}
        }
    }

    pub fn retry_window(&mut self, page: Page) {
        match &page {
            Page::Playlist(id) => {
                let Some(playlist) = self.playlist_pages.get_mut(id) else {
                    return;
                };
                if playlist.pending_writes > 0 || playlist.optimistic_snapshot.is_some() {
                    // A failed snapshot confirmation must retry that confirmation
                    // before any replacement rows can be requested.
                    self.reload(page);
                    return;
                }
                if playlist.items.error.is_none() || !playlist.items.can_load_more() {
                    return;
                }
                playlist.items.error = None;
            }
            Page::Album(id) => {
                let Some(album) = self.album_pages.get_mut(id) else {
                    return;
                };
                if album.tracks.error.is_none() || !album.tracks.can_load_more() {
                    return;
                }
                album.tracks.error = None;
            }
            _ => return,
        }
        self.load_more(page);
    }

    pub fn load_playlist_items_at(&mut self, id: &str, offset: u32) {
        let generation = {
            let Some(page) = self.playlist_pages.get_mut(id) else {
                return;
            };
            self.load_generation += 1;
            page.generation = self.load_generation;
            let total = page.items.total;
            page.items.reset_at(offset);
            page.items.total = total;
            page.items.loading = true;
            page.tail_checked = false;
            page.cache_restored_through = None;
            page.pending_cache = None;
            page.cache_checked = true;
            page.cache_append_valid = false;
            page.local_additions.clear();
            page.optimistic_snapshot = None;
            page.snapshot_rechecks = 0;
            page.refresh_after_write = false;
            page.generation
        };
        self.clear_picked_rows();
        self.backend.api(ApiRequest::PlaylistItems {
            id: id.to_string(),
            offset,
            generation,
        });
    }



    pub fn poll_remote(&mut self, _immediate: bool) {
        if !self.is_connected() {
            return;
        }
        self.remote_poll_pending = true;
        self.remote_polled_at = Instant::now();
        self.remote_poll_seq += 1;
        self.backend.api(ApiRequest::PlaybackState {
            seq: self.remote_poll_seq,
        });
    }

    pub fn refresh_devices(&mut self) {
        if !self.is_connected() || self.devices_loading {
            return;
        }
        self.devices_loading = true;
        self.backend.api(ApiRequest::Devices);
    }

    pub fn refresh_queue(&mut self, force: bool) {
        if !self.is_connected() || !self.network_reachable {
            return;
        }
        if self.resume_only() && matches!(self.queue, Loadable::Loaded(_)) {
            // Nothing is playing anywhere and the queue on show is the
            // remembered one; a fetch could only replace it with less.
            return;
        }
        if self.queue.is_loading() && !force {
            return;
        }
        if !matches!(self.queue, Loadable::Loaded(_)) {
            self.queue = Loadable::Loading;
        }
        self.queue_fetched_at = Some(Instant::now());
        self.queue_seq += 1;
        self.backend.api(ApiRequest::Queue {
            seq: self.queue_seq,
        });
    }

    /// A chosen row of Next up plays at once, and the rows above it go
    /// with it: skips consume the queue, so the playing context and the
    /// songs queued after the chosen one stay intact. Loading the queue's
    /// rows as a fresh list instead used to take seconds, threw the
    /// context away, and left Spotify's copy of the queue to reappear.
    pub fn play_queue_item(&mut self, index: usize, uri: String) {
        if self.resume_only() {
            // Nothing is playing anywhere, so there is no live queue to
            // consume: play the shown rows as a plain list.
            let uris: Vec<String> = self.queue
                .get()
                .map(|queue| {
                    queue
                        .queue
                        .iter()
                        .map(|item| item.uri().to_string())
                        .collect()
                })
                .unwrap_or_default();
            if uris.is_empty() {
                return;
            }
            let (uris, index) = cap_uris(&uris, index as u32);
            self.play_request(PlayRequest::tracks(uris).starting_at_index(index), false);
            return;
        }
        let mut skips = index + 1;
        let mut consumed: Vec<String> = Vec::new();
        if let Loadable::Loaded(queue) = &mut self.queue {
            // The click names a song; if the rows shifted under the
            // pointer, the song wins over the row number.
            let position = match queue.queue.get(index) {
                Some(item) if item.uri() == uri => Some(index),
                _ => queue.queue.iter().position(|item| item.uri() == uri),
            };
            let Some(position) = position else {
                self.refresh_queue(true);
                return;
            };
            skips = position + 1;
            let mut items: Vec<_> = queue.queue.drain(..=position).collect();
            let chosen = items.pop().expect("the chosen row was just drained");
            consumed = items.iter().map(|item| item.uri().to_string()).collect();
            consumed.push(chosen.uri().to_string());
            queue.currently_playing = Some(chosen);
        }
        for gone in &consumed {
            if let Some(at) = self.manual_queue.iter().position(|queued| queued == gone) {
                self.remove_manual_queue_row(at);
            }
        }
        self.expect_track(uri.clone(), 0);
        self.queue_start_pending = Some(self.target());
        self.set_play_pending(vec![uri]);
        self.optimistic_playing = Some((true, Instant::now()));
        match self.target() {
            Target::Local => {
                for _ in 0..skips {
                    self.backend.player(PlayerCommand::Next);
                }
            }
            Target::Remote(device_id) => {
                // With nothing to act on, one call earns the "pick
                // something first" toast; a skip per row would repeat it.
                if device_id.is_none() && self.remote_fresh().is_none() {
                    self.remote(RemoteAction::Next, None);
                    return;
                }
                for _ in 0..skips {
                    self.remote(RemoteAction::Next, device_id.clone());
                }
            }
        }
    }

    /// How many leading rows of Next up are songs the user queued here,
    /// so the view can give them their own section.
    pub fn queued_rows_len(&self) -> usize {
        // Before resume, use the saved manual queue to split restored rows.
        let manual = if self.manual_queue.is_empty() && self.resume_only() {
            &self.resume_queue
        } else {
            &self.manual_queue
        };
        match &self.queue {
            Loadable::Loaded(queue) => Self::end_of_queued_rows(&queue.queue, manual),
            _ => 0,
        }
    }

    /// Whether the local player is the active target, so its queue can be
    /// rewritten directly. Neither the Web API nor librespot can reorder or
    /// insert into a live queue; the only way to change one is to clear it
    /// and re-add its songs in the new order, which only reaches the
    /// engine actually playing them.
    pub fn queue_locally_reorderable(&self) -> bool {
        self.local.is_active() && matches!(self.target(), Target::Local)
    }

    /// Whether the active local queue has rows that can be cleared.
    pub fn can_clear_queue(&self) -> bool {
        self.queue_locally_reorderable() && self.queued_rows_len() > 0
    }

    /// Whether the queue row at `index` is one of the manually queued
    /// songs the active local queue lets the user take out.
    pub fn can_remove_from_queue(&self, index: usize) -> bool {
        self.queue_locally_reorderable() && index < self.queued_rows_len()
    }

    /// Takes the chosen rows out of Playing next. Neither the Web API nor
    /// librespot can remove one queued song, so like a move this rewrites
    /// the local engine's queue, and only that one. A row whose index no
    /// longer holds its song is left alone.
    pub fn remove_from_queue(&mut self, rows: Vec<(usize, String)>) {
        if !self.queue_locally_reorderable() {
            return;
        }
        let queued_len = self.queued_rows_len();
        let Loadable::Loaded(queue) = &self.queue else {
            return;
        };
        let mut indexes: Vec<usize> = rows
            .into_iter()
            .filter(|(index, uri)| {
                *index < queued_len
                    && queue
                        .queue
                        .get(*index)
                        .is_some_and(|item| item.uri() == uri)
            })
            .map(|(index, _)| index)
            .collect();
        indexes.sort_unstable();
        indexes.dedup();
        let mut removed = std::collections::HashSet::new();
        // From the bottom up, so the rows still to go keep their indexes.
        for index in indexes.into_iter().rev() {
            let Loadable::Loaded(queue) = &mut self.queue else {
                return;
            };
            let uri = queue.queue[index].uri().to_string();
            // Copies of one song are interchangeable: the shown row's
            // occurrence among the queued rows is the same occurrence in
            // `manual_queue`, whatever order Spotify last reported.
            let occurrence = queue.queue[..index]
                .iter()
                .filter(|item| item.uri() == uri)
                .count();
            let Some(manual) = self.manual_queue
                .iter()
                .enumerate()
                .filter(|(_, queued)| **queued == uri)
                .nth(occurrence)
                .map(|(manual, _)| manual)
            else {
                continue;
            };
            queue.queue.remove(index);
            self.remove_manual_queue_row(manual);
            removed.insert(uri);
        }
        if removed.is_empty() {
            return;
        }
        if let Some((earlier, at)) = self.queue_removed.take()
            && at.elapsed() < PLAYBACK_HOLD
        {
            removed.extend(earlier);
        }
        self.queue_removed = Some((removed, Instant::now()));
        self.resync_local_queue();
    }

    /// Clears manually queued tracks while keeping the context's upcoming rows.
    pub fn clear_queue(&mut self) {
        if !matches!(self.target(), Target::Local) {
            return;
        }
        self.pending_album_queues.clear();
        self.last_album_queue = None;
        // `queue_one` writes every queued song to both lists, so they hold
        // the same wishes and adding their counts asks for twice the rows
        // Next up was given. The extra row taken is the context's own copy
        // of that song, which stays. Neither list alone is the count
        // either: a pending add outlives its `manual_queue` entry once the
        // song starts.
        // Take as many rows as the longer of the two holds.
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for uri in &self.manual_queue {
            *counts.entry(uri.clone()).or_insert(0) += 1;
        }
        let mut pending: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for addition in &self.pending_queue_adds {
            *pending.entry(addition.item.uri()).or_insert(0) += 1;
        }
        for (uri, count) in pending {
            let held = counts.entry(uri.to_string()).or_insert(0);
            *held = (*held).max(count);
        }
        if let Loadable::Loaded(queue) = &mut self.queue {
            // Remove one row per queued copy, starting at the front.
            queue.queue.retain(|item| match counts.get_mut(item.uri()) {
                Some(left) if *left > 0 => {
                    *left -= 1;
                    false
                }
                _ => true,
            });
        }
        let cleared: std::collections::HashSet<String> = counts.into_keys().collect();
        if !self.manual_queue.is_empty() {
            self.session_dirty = true;
        }
        self.manual_queue.clear();
        self.pending_queue_adds.clear();
        if !cleared.is_empty() {
            self.queue_cleared = Some((cleared, Instant::now()));
        }
        self.backend.player(PlayerCommand::ClearQueue);
        // Refresh to remove queued tracks added by another client.
        self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
        self.toast(self.localize.t(self.locale, "Queue cleared"));
    }

    /// Current and upcoming track URIs, deduplicated in playback order.
    pub fn queue_playlist_uris(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut uris = Vec::new();
        let queued = self.queue.get();
        let rows = self.now_playing().map(|now| now.uri).into_iter().chain(
            queued
                .iter()
                .flat_map(|queue| queue.queue.iter().map(|item| item.uri().to_string())),
        );
        for uri in rows {
            if seen.insert(uri.clone()) {
                uris.push(uri);
            }
        }
        uris
    }

    /// Name for a playlist created from the queue.
    pub fn queue_playlist_name(&self) -> String {
        if let Some(context) = self.playing_context_uri()
            && let Some(name) = self.station_name(&context)
        {
            return name;
        }
        let today = jiff::Zoned::now().strftime("%Y-%m-%d").to_string();
        // Translators: Keep {date} unchanged. It is a date in YYYY-MM-DD form.
        self.localize.t(self.locale, "Queue {date}").replace("{date}", &today)
    }

    /// Saves the queue as a new playlist.
    pub fn save_queue_as_playlist(&mut self) {
        let uris = self.queue_playlist_uris();
        if uris.is_empty() {
            return;
        }
        let name = self.queue_playlist_name();
        self.actions.push(Action::CreatePlaylist {
            name,
            public: false,
            add_uris: uris,
        });
    }

    /// Shows the head of Next up as playing before the engine confirms it.
    pub fn pop_queue_head(&mut self) {
        // A restored or stale queue has no trustworthy relationship to the
        // current player state. It can stay visible while the live queue is
        // fetched, but must not be used to guess where Next will land.
        let current = self.current_track_uri();
        let Loadable::Loaded(queue) = &mut self.queue else {
            return;
        };
        let anchored =
            current.as_deref() == queue.currently_playing.as_ref().map(PlayableItem::uri);
        if !anchored || queue.queue.is_empty() {
            return;
        }
        let item = queue.queue.remove(0);
        let uri = item.uri().to_string();
        // The queue row already has the song's details, so the player bar
        // can name it now instead of waiting for the engine to load it.
        let track = match &item {
            crate::api::models::PlayableItem::Track(track) => Some(track.clone()),
            crate::api::models::PlayableItem::Episode(_) => None,
        };
        queue.currently_playing = Some(item);
        if let Some(track) = track
            && let Some(id) = util::uri_id(&uri)
        {
            self.track_cache.entry(id.to_owned()).or_insert(track);
        }
        self.consume_manual_queue_head(&uri);
        self.queue_start_pending = Some(self.target());
        self.expect_track(uri, 0);
    }

    /// Consume one queued occurrence, retaining any later copy of this song.
    pub fn consume_manual_queue_head(&mut self, uri: &str) {
        if self.manual_queue.first().map(String::as_str) == Some(uri) {
            self.remove_manual_queue_row(0);
        }
    }

    pub fn remove_manual_queue_row(&mut self, index: usize) {
        self.manual_queue.remove(index);
        self.pending_queue_adds.retain_mut(|addition| {
            if addition.manual_index == index {
                return false;
            }
            if addition.manual_index > index {
                addition.manual_index -= 1;
            }
            true
        });
        self.session_dirty = true;
    }

    /// Whether a fetched queue predates the latest local change.
    pub fn queue_fetch_is_stale(&self, fetched: &Queue) -> bool {
        if self.queue_stale_retries >= QUEUE_STALE_RETRIES {
            return false;
        }
        // A recent play or pop must be reflected in the fetched current row.
        if let Some(intent) = &self.intent_track
            && (intent.at.elapsed() < PLAYBACK_HOLD || self.queued_play.is_some())
            && fetched
                .currently_playing
                .as_ref()
                .is_none_or(|item| item.uri() != intent.uri)
        {
            return true;
        }
        // For local playback, reject a fetched current row that differs from
        // the engine's current track.
        if self.local.is_active()
            && let Some(track) = &self.local.track
            && fetched
                .currently_playing
                .as_ref()
                .is_some_and(|item| item.uri() != track.uri)
        {
            return true;
        }
        // A cleared row still on top means the clear has not landed yet.
        if let Some((cleared, at)) = &self.queue_cleared
            && at.elapsed() < PLAYBACK_HOLD
            && fetched
                .queue
                .first()
                .is_some_and(|item| cleared.contains(item.uri()))
        {
            return true;
        }
        // An unchanged context order right after toggling shuffle means the
        // reordered queue has not landed yet.
        if let Some(pending) = &self.queue_shuffle_pending
            && pending.at.elapsed() < PLAYBACK_HOLD
            && fetched.currently_playing.as_ref().map(|item| item.uri())
                == pending.current_uri.as_deref()
        {
            let at = Self::end_of_queued_rows(&fetched.queue, &self.manual_queue);
            let fetched_context = &fetched.queue[at..];
            if fetched_context.len() == pending.context_uris.len()
                && fetched_context
                    .iter()
                    .map(|item| item.uri())
                    .eq(pending.context_uris.iter().map(String::as_str))
            {
                return true;
            }
        }
        // A local reorder or positional insert is optimistic; reject a
        // fetch whose queued rows have not caught up to it yet.
        if let Some(at) = self.queue_reorder_pending
            && at.elapsed() < PLAYBACK_HOLD
            && !fetched
                .queue
                .iter()
                .take(self.manual_queue.len())
                .map(|item| item.uri())
                .eq(self.manual_queue.iter().map(String::as_str))
        {
            return true;
        }
        // A removed song still right after the queued rows means the
        // removal has not landed yet: the prefix alone cannot tell, since
        // taking out the last queued row leaves the rest of it unchanged.
        if let Some((removed, at)) = &self.queue_removed
            && at.elapsed() < PLAYBACK_HOLD
            && fetched
                .queue
                .get(self.manual_queue.len())
                .is_some_and(|item| removed.contains(item.uri()))
        {
            return true;
        }
        false
    }

    pub fn run_search(&mut self, query: String) {
        if !query.is_empty()
            && query == self.search.committed
            && !self.search.results.needs_load()
            && self.search.error.is_none()
        {
            return;
        }
        self.search.serial += 1;
        self.search.committed = query.clone();
        self.search.committed_run = None;
        self.search.playlists = None;
        self.search.error = None;
        self.search.catalogue_pending = !query.is_empty();
        self.search.playlists_pending = false;
        // Results and their query change together. In particular, a failed
        // new query must never inherit the previous query's songs.
        self.search.results = if query.is_empty() {
            Loadable::NotLoaded
        } else {
            Loadable::Loading
        };
        // An empty query also cancels requests waiting on Spotify's quota.
        self.backend.api(ApiRequest::Search {
            query,
            serial: self.search.serial,
        });
    }

    /// A search the debounce started while the user may still be typing.
    pub fn run_typed_search(&mut self, query: String) {
        let serial = self.search.serial;
        self.run_search(query);
        if self.search.serial != serial {
            self.search.committed_run = Some((self.search.edit_run, true));
        }
    }

    /// Searches for what the field holds, ending its run of typing.
    pub fn submit_typed_search(&mut self, query: String) {
        let run = self.search.edit_run;
        self.search.edit_run += 1;
        let serial = self.search.serial;
        self.run_search(query);
        if self.search.serial != serial
            || self.search.catalogue_pending
            || self.search.playlists_pending
        {
            self.search.committed_run = Some((run, false));
        }
    }

    /// Records a query that found results. While typing, a pause commits
    /// each prefix in turn, so a query that extends or shortens the one the
    /// same run recorded takes its place instead of adding another. Only an
    /// entry the run itself added is replaced.
    pub fn remember_search(&mut self, query: &str) {
        let query = query.trim();
        if let Some((run, typing)) = self.search.committed_run
            && self.search.typed_entry != Some((run, query.to_string()))
        {
            if let Some((entry_run, entry)) = self.search.typed_entry.take()
                && entry_run == run
                && (query.starts_with(entry.as_str()) || entry.starts_with(query))
                && self.settings.search_history.first() == Some(&entry)
            {
                self.settings.search_history.remove(0);
            }
            let added = !self.settings
                .search_history
                .iter()
                .any(|entry| entry == query);
            self.search.typed_entry = (typing && added).then(|| (run, query.to_string()));
        }
        self.settings.remember_search(query);
        self.settings_dirty = true;
    }

    pub fn show_search_playlists(&mut self) {
        let Some((serial, page)) = self.search.playlists.as_ref() else {
            return;
        };
        if *serial != self.search.serial {
            return;
        }
        if self.search.results_serial != *serial || self.search.results.get().is_none() {
            self.search.results = Loadable::Loaded(Default::default());
            self.search.results_serial = *serial;
        }
        if let Some(results) = self.search.results.get_mut() {
            results.playlists = Some(page.clone());
        }
    }

    pub fn search_failed(&mut self, part: &str, error: impl std::fmt::Display) {
        let message = format!("{part}: {error}");
        self.search.error = Some(match self.search.error.take() {
            Some(previous) => format!("{previous}\n{message}"),
            None => message,
        });
        if self.search.results_serial != self.search.serial
            && !self.search.catalogue_pending
            && !self.search.playlists_pending
        {
            self.search.results = Loadable::Failed(self.search.error.clone().unwrap());
        }
    }

    /// Resolves user IDs that do not have a cached display name.
    pub fn request_user_names(&mut self, ids: Vec<String>) {
        let unknown: Vec<String> = ids
            .into_iter()
            .filter(|id| !self.user_names.contains_key(id))
            .collect();
        if unknown.is_empty() {
            return;
        }
        for id in &unknown {
            self.user_names.insert(id.clone(), None);
        }
        self.backend.send(Command::UserNames(unknown));
    }

    pub fn request_album_types<'a>(&mut self, albums: impl IntoIterator<Item = &'a Album>) {
        let mut uris = Vec::new();
        for album in albums {
            if album.is_single_release()
                && !album.uri.is_empty()
                && self.album_types_requested.insert(album.uri.clone())
            {
                uris.push(album.uri.clone());
            }
        }
        if !uris.is_empty() {
            self.backend.album_types(uris);
        }
    }

    pub fn request_contains(&mut self, uris: Vec<String>) {
        let mut batch = Vec::new();
        for uri in uris {
            if uri.is_empty()
                || self.saved.contains_key(&uri)
                || self.saved_pending.contains(&uri)
                || uri.starts_with("spotify:local")
            {
                continue;
            }
            self.saved_pending.insert(uri.clone());
            batch.push(uri);
            if batch.len() == CONTAINS_BATCH {
                self.backend.api(ApiRequest::Contains {
                    uris: std::mem::take(&mut batch),
                });
            }
        }
        if !batch.is_empty() {
            self.backend.api(ApiRequest::Contains { uris: batch });
        }
    }

    // ---- api responses -------------------------------------------------------

    pub fn cover_chosen(
        &mut self,
        id: &str,
        request: u64,
        result: Result<Option<crate::playlist_cover::Cover>, String>,
    ) {
        if let Some(Dialog::EditPlaylist {
            id: current, cover, ..
        }) = &mut self.dialog
            && current == id
            && cover.request == Some(request)
        {
            cover.request = None;
            match result {
                Ok(Some(selected)) => {
                    cover.selection = Some(selected);
                    cover.error = None;
                }
                Ok(None) => {}
                Err(error) => cover.error = Some(error),
            }
        }
    }

    pub fn set_playlist_cover(&mut self, id: &str, cover: &crate::playlist_cover::Cover) {
        if let Some(page) = self.playlist_pages.get_mut(id)
            && let Some(playlist) = page.playlist.get_mut()
        {
            playlist.images = cover_images(cover);
        }
        if let Some(playlists) = self.library.playlists.get_mut() {
            for playlist in playlists.iter_mut().filter(|playlist| playlist.id == id) {
                playlist.images = cover_images(cover);
            }
        }
    }

    pub fn reconcile_playlist_cover(&mut self, id: &str, images: &mut Vec<crate::api::models::Image>) {
        let Some(pending) = self.uploaded_covers.get_mut(id) else {
            return;
        };
        let new_urls = !images.is_empty()
            && images
                .iter()
                .all(|image| !pending.previous_urls.contains(&image.url));
        if new_urls {
            if pending.checking.is_none() {
                pending.checking = Some(images.clone());
                self.backend.send(Command::CheckPlaylistCover {
                    id: id.into(),
                    request: pending.request,
                    cover: pending.cover.clone(),
                    images: images.clone(),
                });
            } else if pending.checking.as_ref() != Some(images) {
                // Keep the latest candidate while one image is being checked.
                // Do not start parallel downloads for every metadata reply.
                pending.next_images = Some(images.clone());
            }
        }
        *images = cover_images(&pending.cover);
        if !new_urls {
            self.recheck_playlist_cover(id);
        }
    }

    pub fn recheck_playlist_cover(&mut self, id: &str) {
        if let Some(pending) = self.uploaded_covers.get_mut(id)
            && pending.rechecks_left > 0
            && let Some(page) = self.playlist_pages.get(id)
        {
            pending.rechecks_left -= 1;
            self.backend.api(ApiRequest::Playlist {
                id: id.into(),
                generation: page.generation,
            });
        }
    }

    pub fn cover_checked(
        &mut self,
        id: &str,
        request: u64,
        images: Vec<crate::api::models::Image>,
        result: Result<bool, String>,
    ) {
        let Some(pending) = self.uploaded_covers.get_mut(id) else {
            return;
        };
        if pending.request != request || pending.checking.as_ref() != Some(&images) {
            return;
        }
        pending.checking = None;
        let next_images = pending.next_images.take();
        let should_recheck = matches!(result, Ok(false));
        match result {
            Ok(true) => {
                self.uploaded_covers.remove(id);
                if let Some(page) = self.playlist_pages.get_mut(id)
                    && let Some(playlist) = page.playlist.get_mut()
                {
                    playlist.images = images.clone();
                }
                if let Some(playlists) = self.library.playlists.get_mut() {
                    for playlist in playlists.iter_mut().filter(|playlist| playlist.id == id) {
                        playlist.images = images.clone();
                    }
                }
            }
            Ok(false) => {
                pending
                    .previous_urls
                    .extend(images.into_iter().map(|image| image.url));
            }
            // The successful upload stays visible if its artwork cannot be
            // checked. A later metadata refresh can try that URL again.
            Err(_) => {}
        }
        if self.uploaded_covers.contains_key(id) {
            if let Some(mut next) = next_images {
                self.reconcile_playlist_cover(id, &mut next);
            } else if should_recheck {
                self.recheck_playlist_cover(id);
            }
        }
    }

    /// Applies an API response to the current page and ignores stale Home data.


    /// Makes the next poll due within 700 ms, whatever the interval: it
    /// counts back from the longest one, so a window hidden meanwhile does
    /// not put a follow-up poll off.
    pub fn poll_remote_soon(&mut self) {
        self.remote_polled_at = Instant::now() - REMOTE_POLL_UNSEEN + Duration::from_millis(700);
    }

    // ---- navigation ------------------------------------------------------------

    pub fn open(&mut self, page: Page) {
        self.pending_link = None;
        self.prerelease_request = None;
        self.touch_page(&page);
        if *self.page() == page {
            self.ensure_loaded(page.clone());
            self.retain_table_rows(&page);
            return;
        }
        self.history.truncate(self.history_index + 1);
        self.history.push(page.clone());
        if self.history.len() > 60 {
            self.history.remove(0);
        }
        self.history_index = self.history.len() - 1;
        self.show_devices = false;
        self.ensure_loaded(page.clone());
        self.retain_table_rows(&page);
        self.evict_stale_pages();
    }

    /// Hands the app a Spotify link from outside, a canonical URI as
    /// [`crate::link::parse`] makes it: the window comes forward and the
    /// page opens once the account is signed in.
    pub fn open_link(&mut self, uri: String) {
        self.actions.push(Action::OpenLink(uri));
    }

    /// Opens the page behind the pending link once the account is signed
    /// in. A song and an episode have no page of their own here, so the
    /// album's and the podcast's open, once Spotify has said which.
    pub fn open_pending_link(&mut self) {
        let Some(uri) = self.pending_link.clone() else {
            return;
        };
        if self.user.is_none() {
            return;
        }
        if let Some(query) = crate::link::search_query(&uri) {
            self.pending_link = None;
            self.actions.push(Action::Search(query));
            self.actions.push(Action::FocusSearch);
            return;
        }
        if let Some(page) = Page::from_uri(&uri) {
            self.pending_link = None;
            self.open(page);
            return;
        }
        let id = util::uri_id(&uri).unwrap_or_default().to_string();
        match util::uri_kind(&uri) {
            Some("prerelease") => {
                if self.prerelease_request.is_none() {
                    self.prerelease_serial = self.prerelease_serial.wrapping_add(1);
                    self.prerelease_request = Some(self.prerelease_serial);
                    self.backend.send(Command::Prerelease {
                        uri,
                        generation: self.prerelease_serial,
                    });
                }
            }
            Some("track") => {
                if let Some(track) = self.read_cached_track(&id) {
                    self.pending_link = None;
                    let album = track
                        .album
                        .as_ref()
                        .map(|album| album.id.clone())
                        .filter(|id| !id.is_empty());
                    match album {
                        Some(album) => self.open(Page::Album(album)),
                        None => self.toast_error(self.localize.t(
                            self.locale,
                            "This song's album is not on Spotify",
                        )),
                    }
                } else if self.track_requests.insert(id.clone()) {
                    // The answer lands in the cache, and the link waits
                    // for the next frame to find it there.
                    self.backend.api(ApiRequest::Track { id });
                }
            }
            Some("episode") => {
                self.pending_link = None;
                self.backend.api(ApiRequest::Episode { id });
            }
            _ => {
                self.pending_link = None;
                self.toast_error(self.localize.t(
                    self.locale,
                    "Spotifast cannot open this kind of Spotify link",
                ));
            }
        }
    }

    pub fn receive_prerelease(
        &mut self,
        uri: &str,
        generation: u64,
        result: Result<crate::prerelease::Prerelease, String>,
    ) {
        if self.user.is_none()
            || self.pending_link.as_deref() != Some(uri)
            || self.prerelease_request != Some(generation)
        {
            return;
        }
        self.pending_link = None;
        self.prerelease_request = None;
        match result {
            Ok(data) => {
                let id = data.album.id.clone();
                self.load_generation = self.load_generation.wrapping_add(1);
                let mut tracks = PagedList::default();
                tracks.absorb(
                    0,
                    crate::api::models::Page {
                        items: data
                            .tracks
                            .iter()
                            .flatten()
                            .filter(|track| track.is_playable == Some(true))
                            .cloned()
                            .collect(),
                        total: data
                            .tracks
                            .iter()
                            .flatten()
                            .filter(|track| track.is_playable == Some(true))
                            .count() as u32,
                        ..Default::default()
                    },
                );
                self.album_pages.insert(
                    id.clone(),
                    AlbumPage {
                        generation: self.load_generation,
                        album: Loadable::Loaded(data.album.clone()),
                        prerelease: Some(data),
                        tracks,
                    },
                );
                self.open(Page::Album(id));
            }
            Err(error) => self.toast_error(error),
        }
    }

    pub fn receive_prerelease_saved(
        &mut self,
        uri: &str,
        saved: bool,
        generation: u64,
        result: Result<(), String>,
    ) {
        for page in self.album_pages
            .values_mut()
            .filter(|page| page.generation == generation)
        {
            if let Some(data) = &mut page.prerelease
                && data.album.uri == uri
                && data.saving
                && data.saved == Some(saved)
            {
                data.saving = false;
                match result {
                    Ok(()) => data.save_error = None,
                    Err(error) => {
                        // A timeout may have committed remotely. Unknown is
                        // more honest than guessing or silently sending again.
                        data.saved = None;
                        data.save_error = Some(error);
                    }
                }
                break;
            }
        }
    }

    pub fn touch_page(&mut self, page: &Page) {
        self.page_used.insert(page.clone(), Instant::now());
    }

    pub fn read_cached_track(&mut self, id: &str) -> Option<Track> {
        let track = self.track_cache.get(id).cloned()?;
        self.track_used.insert(id.to_owned(), Instant::now());
        Some(track)
    }

    /// Drops page caches that exceed the cap, keeping the open page, the
    /// playing context, and the most recently used pages. History does not
    /// protect a page from the cap. Track metadata is an LRU of 800.
    /// Table-row copies of dropped playlist and album pages go with them.
    pub fn evict_stale_pages(&mut self) {
        const MAX_PLAYLIST_PAGES: usize = 12;
        const MAX_ALBUM_PAGES: usize = 16;
        const MAX_ARTIST_PAGES: usize = 10;
        const MAX_SHOW_PAGES: usize = 8;
        const MAX_RADIO_PAGES: usize = 6;
        const MAX_TRACK_CACHE: usize = 800;

        let mut protected_playlists = HashSet::new();
        for (id, page) in &self.playlist_pages {
            if page.pending_writes > 0 || page.optimistic_snapshot.is_some() {
                protected_playlists.insert(id.clone());
            }
        }
        let mut protected_albums = HashSet::new();
        let mut protected_artists = HashSet::new();
        let mut protected_shows = HashSet::new();
        let mut protected_radios = HashSet::new();
        if let Some(seed) = self
            .playing_context_uri()
            .and_then(|uri| util::station_seed(&uri))
        {
            protected_radios.insert(seed);
        }
        match self.page() {
            Page::Playlist(id) => {
                protected_playlists.insert(id.clone());
            }
            Page::Album(id) => {
                protected_albums.insert(id.clone());
            }
            Page::Artist(id) => {
                protected_artists.insert(id.clone());
            }
            Page::Show(id) => {
                protected_shows.insert(id.clone());
            }
            Page::Radio(seed) => {
                protected_radios.insert(seed.clone());
            }
            _ => {}
        }
        if let Some(uri) = self.playing_context_uri()
            && let Some(kind) = util::uri_kind(&uri)
            && let Some(id) = util::uri_id(&uri)
        {
            match kind {
                "playlist" => {
                    protected_playlists.insert(id.to_string());
                }
                "album" => {
                    protected_albums.insert(id.to_string());
                }
                "artist" => {
                    protected_artists.insert(id.to_string());
                }
                "show" => {
                    protected_shows.insert(id.to_string());
                }
                _ => {}
            }
        }
        evict_lru_map(
            &mut self.playlist_pages,
            &self.page_used,
            |id| Page::Playlist(id.to_string()),
            &protected_playlists,
            MAX_PLAYLIST_PAGES,
        );
        evict_lru_map(
            &mut self.album_pages,
            &self.page_used,
            |id| Page::Album(id.to_string()),
            &protected_albums,
            MAX_ALBUM_PAGES,
        );
        evict_lru_map(
            &mut self.artist_pages,
            &self.page_used,
            |id| Page::Artist(id.to_string()),
            &protected_artists,
            MAX_ARTIST_PAGES,
        );
        evict_lru_map(
            &mut self.show_pages,
            &self.page_used,
            |id| Page::Show(id.to_string()),
            &protected_shows,
            MAX_SHOW_PAGES,
        );
        evict_lru_map(
            &mut self.radio_pages,
            &self.page_used,
            |seed| Page::Radio(seed.to_string()),
            &protected_radios,
            MAX_RADIO_PAGES,
        );
        self.page_used.retain(|page, _| match page {
            Page::Playlist(id) => self.playlist_pages.contains_key(id),
            Page::Album(id) => self.album_pages.contains_key(id),
            Page::Artist(id) => self.artist_pages.contains_key(id),
            Page::Show(id) => self.show_pages.contains_key(id),
            Page::Radio(seed) => self.radio_pages.contains_key(seed),
            _ => true,
        });
        if self.track_cache.len() > MAX_TRACK_CACHE {
            let playing = self.now_playing().and_then(|now| now.id);
            let overflow = self.track_cache.len() - MAX_TRACK_CACHE;
            let mut victims: Vec<(Option<Instant>, String)> = self.track_cache
                .keys()
                .filter(|id| playing.as_ref() != Some(*id))
                .map(|id| {
                    let used = self.track_used.get(id).copied();
                    (used, id.clone())
                })
                .collect();
            victims.sort();
            for (_, id) in victims.into_iter().take(overflow) {
                self.track_cache.remove(&id);
                self.track_used.remove(&id);
            }
        }
        let current = self.page().clone();
        self.retain_table_rows(&current);
    }

    // ---- playback --------------------------------------------------------------

    pub fn remote(&mut self, action: RemoteAction, device_id: Option<String>) {
        if device_id.is_none() && self.remote_fresh().is_none() {
            // Spotify would only answer "no active device found".
            self.clear_play_pending();
            self.toast(self.localize.t(
                self.locale,
                "Nothing is playing. Pick something first",
            ));
            return;
        }
        self.backend.api(ApiRequest::Remote {
            action,
            device_id,
            play: None,
            position_ms: 0,
            percent: 0,
            flag: false,
            repeat: String::new(),
        });
    }

    /// Remembers `uri` as the most recently played context, for the
    /// sidebar's order.
    pub fn note_recent_context(&mut self, uri: &str) {
        self.session_dirty = true;
        if !Self::is_sidebar_context(uri) {
            return;
        }
        self.recent_contexts.retain(|held| held != uri);
        self.recent_contexts.insert(0, uri.to_string());
        self.recent_contexts.truncate(RECENT_CONTEXTS_KEPT);
    }

    /// Whether the sidebar lists `uri`, so its order has a place for it.
    pub fn is_sidebar_context(uri: &str) -> bool {
        uri.contains(":playlist:") || uri.contains(":album:") || uri.contains(":collection")
    }

    /// Notes the contexts of a page of history older than every play the
    /// order already holds. They go after it, newest first, and a context
    /// already in the order keeps the place a newer play gave it.
    pub fn note_older_contexts(&mut self, history: &[crate::api::models::PlayHistory]) {
        for play in history {
            let Some(uri) = play.context.as_ref().map(|context| context.uri.as_str()) else {
                continue;
            };
            if self.recent_contexts.len() >= RECENT_CONTEXTS_KEPT {
                break;
            }
            if Self::is_sidebar_context(uri) && !self.recent_contexts.iter().any(|held| held == uri)
            {
                self.recent_contexts.push(uri.to_string());
                self.session_dirty = true;
            }
        }
    }

    /// Notes every context in a page of play history, oldest first, so
    /// the newest ends up at the front of the sidebar's order.
    pub fn note_recent_contexts(&mut self, history: &[crate::api::models::PlayHistory]) {
        let contexts: Vec<String> = history
            .iter()
            .rev()
            .filter_map(|play| play.context.as_ref().map(|context| context.uri.clone()))
            .collect();
        for context in contexts {
            self.note_recent_context(&context);
        }
    }

    /// Merges local and Spotify history for the Recent tab.
    pub fn rebuild_recents(&mut self) {
        self.recents_view = crate::history::merged(self.plays.plays(), &self.recents.items);
    }

    /// Adds a page of play history to the Recents tab.
    ///
    /// Repeated plays remain separate. Only identical track-and-time entries
    /// are deduplicated across page boundaries.
    ///
    /// Spotify paginates backwards with `before`; a short page ends the list.
    pub fn absorb_recents(
        &mut self,
        page: crate::api::models::CursorPage<crate::api::models::PlayHistory>,
        limit: u32,
    ) {
        // A page asked for with a cursor is older than the pages before it.
        if self.recents.after.is_some() {
            self.note_older_contexts(&page.items);
        } else {
            self.note_recent_contexts(&page.items);
        }
        self.recents.error = None;
        let short_page = (page.items.len() as u32) < limit;
        let cursor = page.cursors.as_ref().and_then(|c| c.before.clone());
        let mut seen: std::collections::HashSet<(String, Option<String>)> = self.recents
            .items
            .iter()
            .map(|play| (play.track.uri.clone(), play.played_at.clone()))
            .collect();
        let fresh: Vec<crate::api::models::PlayHistory> = page
            .items
            .into_iter()
            .filter(|play| seen.insert((play.track.uri.clone(), play.played_at.clone())))
            .collect();
        let uris: Vec<String> = fresh.iter().map(|play| play.track.uri.clone()).collect();
        self.recents.items.extend(fresh);
        match cursor {
            Some(cursor) if !short_page => self.recents.after = Some(cursor),
            _ => {
                self.recents.complete = true;
                self.recents.after = None;
            }
        }
        // Request saved state only for newly added rows.
        if !uris.is_empty() {
            self.request_contains(uris);
        }
        self.rebuild_recents();
    }

    /// Loaded track URIs for a context, in display order.
    pub fn context_track_uris(&self, context_uri: &str) -> Option<Vec<String>> {
        let uris: Vec<String> = if let Some(id) = context_uri.strip_prefix("spotify:playlist:") {
            self.playlist_pages
                .get(id)?
                .items
                .items
                .iter()
                .filter_map(|item| item.playable())
                .map(|item| item.uri().to_string())
                .collect()
        } else if let Some(id) = context_uri.strip_prefix("spotify:album:") {
            self.album_pages
                .get(id)?
                .tracks
                .items
                .iter()
                .filter(|track| !track.uri.is_empty())
                .map(|track| track.uri.clone())
                .collect()
        } else if context_uri.ends_with(":collection") {
            self.library
                .liked
                .items
                .iter()
                .map(|item| item.track.uri.clone())
                .collect()
        } else {
            return None;
        };
        (!uris.is_empty()).then_some(uris)
    }

    pub fn random_track_in(&self, context_uri: &str) -> Option<String> {
        let uris = self.context_track_uris(context_uri)?;
        Some(uris[rand::random_range(0..uris.len())].clone())
    }

    /// Last known context length from the library.
    /// Used to choose a random shuffle offset before rows are loaded.
    pub fn context_len(&self, context_uri: &str) -> Option<u32> {
        if context_uri.ends_with(":collection") {
            return self.library.liked.total;
        }
        match util::uri_kind(context_uri)? {
            "playlist" => self.library
                .playlists
                .get()?
                .iter()
                .find(|playlist| playlist.uri == context_uri)?
                .tracks
                .as_ref()
                .map(|tracks| tracks.total),
            "album" => {
                self.library
                    .albums
                    .items
                    .iter()
                    .find(|saved| saved.album.uri == context_uri)?
                    .album
                    .total_tracks
            }
            "show" => {
                self.library
                    .shows
                    .items
                    .iter()
                    .find(|saved| saved.show.uri == context_uri)?
                    .show
                    .total_episodes
            }
            _ => None,
        }
    }

    /// Where a shuffled play of `context_uri` begins, as an offset by
    /// track URI or by position: a random one of the rows the app holds,
    /// or, when it holds none, a random position within the length the
    /// library knows. Neither, and the play carries no offset at all:
    /// librespot then picks its own random track, and only the Web API,
    /// which would start at track one, needs telling where to go.
    pub fn shuffle_start(&self, context_uri: &str) -> (Option<String>, Option<u32>) {
        if let Some(uri) = self.random_track_in(context_uri) {
            return (Some(uri), None);
        }
        if matches!(self.target(), Target::Remote(Some(_)))
            && let Some(len) = self.context_len(context_uri)
            && len > 0
        {
            return (None, Some(rand::random_range(0..len)));
        }
        (None, None)
    }

    /// Start a playlist at its first available row, not an unspecified place
    /// in the player's resolved context. A loaded range from the middle is
    /// not the playlist's beginning; without its prefix, request position zero.
    pub fn playlist_start(&self, id: &str) -> (Option<String>, Option<u32>) {
        let first = self.playlist_pages
            .get(id)
            .filter(|page| page.items.base_offset == 0)
            .and_then(|page| {
                page.items.items.iter().find_map(|row| {
                    let item = row.playable()?;
                    if row.is_local
                        || item.uri().is_empty()
                        || matches!(item, PlayableItem::Track(track)
                            if track.is_local || track.is_playable == Some(false))
                    {
                        return None;
                    }
                    Some(item.uri().to_string())
                })
            });
        match first {
            Some(uri) => (Some(uri), None),
            None => (None, Some(0)),
        }
    }

    /// With `shuffle_first`, shuffle is turned on before playback starts,
    /// in one ordered exchange: two independent requests race, and shuffle
    /// sometimes lost.
    pub fn play_request(&mut self, request: PlayRequest, shuffle_first: bool) {
        // Whatever starts now outranks a play-search still in flight.
        self.supersede_play_search();
        self.start_play(request, shuffle_first);
    }

    /// A playback choice made now: a play-search still in flight must not
    /// undo it when it answers.
    pub fn supersede_play_search(&mut self) {
        self.play_search_serial = self.play_search_serial.wrapping_add(1);
    }

    /// Replays a play that waited for the engine or the session. It keeps
    /// the precedence it took when it was asked for, so a play-search sent
    /// while it waited still plays its match.
    pub fn replay_queued_play(&mut self) {
        if let Some(request) = self.queued_play.take() {
            self.start_play(request, false);
        }
    }

    pub fn start_play(&mut self, request: PlayRequest, shuffle_first: bool) {
        // Shuffle applies across contexts until disabled. A selected row still
        // starts first; otherwise choose a random starting track.
        let mut request = request;
        self.queue_shuffle_pending = None;
        if shuffle_first {
            self.shuffle_wanted = true;
            self.shuffle_set_at = Some(Instant::now());
            self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
        }
        let shuffle = shuffle_first || self.shuffle_wanted;
        if request.offset_uri.is_none()
            && request.offset_position.is_none()
            && request.uris.is_empty()
            && let Some(context) = request.context_uri.as_deref()
        {
            if shuffle {
                (request.offset_uri, request.offset_position) = self.shuffle_start(context);
            } else if let Some(id) = context.strip_prefix("spotify:playlist:") {
                (request.offset_uri, request.offset_position) = self.playlist_start(id);
            }
        }
        let mut keys: Vec<String> = Vec::new();
        if let Some(context) = &request.context_uri {
            keys.push(context.clone());
        }
        if let Some(offset) = &request.offset_uri {
            keys.push(offset.clone());
        }
        match request.offset_position {
            // The play starts at a chosen row; only that row is starting.
            Some(position) => {
                if let Some(uri) = request.uris.get(position as usize) {
                    keys.push(uri.clone());
                }
            }
            // No chosen row: the list starts at its first song.
            None if request.offset_uri.is_none() => {
                if let Some(first) = request.uris.first() {
                    keys.push(first.clone());
                }
            }
            None => {}
        }
        let expected_track = keys.iter().find(|key| key.contains(":track:")).cloned();
        if let Some(uri) = expected_track {
            if let Some(context) = &request.context_uri {
                self.cache_track_from_context(context, &uri);
            }
            self.expect_track(uri, request.position_ms);
        } else {
            self.intent_track = None;
        }
        self.set_play_pending(keys);
        if let Some(context) = request.context_uri.clone() {
            self.note_recent_context(&context);
        }
        // Show a context as playing at once, or clear the previous context
        // for a plain track list. Spotify's state catches up behind it.
        self.assumed_context = Some(AssumedContext {
            uri: request.context_uri.clone().unwrap_or_default(),
            shuffle: shuffle.then_some(true),
            at: Instant::now(),
        });
        self.queue_start_pending = Some(self.target());
        match self.target() {
            Target::Local if !self.local.connected => {
                // Hold the request while the local engine reconnects.
                self.queued_play = Some(request);
            }
            Target::Local => {
                self.queued_play = None;
                let mut load = local_load(&request, shuffle);
                load.repeat = Some(self.local.repeat);
                self.local_list = load.context_uri.is_none().then(|| load.uris.clone());
                let shuffle_after = shuffle && load.shuffle.is_none() && !load.uris.is_empty();
                self.backend.player(PlayerCommand::Load(load));
                if shuffle_after {
                    self.backend.player(PlayerCommand::Shuffle(true));
                }
                self.optimistic_playing = Some((true, Instant::now()));
            }
            Target::Remote(Some(device_id)) => {
                self.queued_play = None;
                if shuffle {
                    self.backend.api(ApiRequest::ShufflePlay {
                        device_id: Some(device_id),
                        play: request,
                    });
                } else {
                    self.backend.api(ApiRequest::Remote {
                        action: RemoteAction::Play,
                        device_id: Some(device_id),
                        play: Some(request),
                        position_ms: 0,
                        percent: 0,
                        flag: false,
                        repeat: String::new(),
                    });
                }
                self.optimistic_playing = Some((true, Instant::now()));
            }
            Target::Remote(None) => {
                // No remote device is active, and this computer's player is
                // not ready. Never ask Spotify to play "nowhere": either
                // wait for the connecting engine or ask for a device.
                if matches!(
                    self.local_playback,
                    LocalPlayback::Connecting
                        | LocalPlayback::Authorizing
                        | LocalPlayback::WaitingForNetwork
                ) || (self.settings.playback_authorized
                    && matches!(self.auth, AuthStatus::Starting | AuthStatus::Connecting))
                {
                    self.queued_play = Some(request);
                } else {
                    self.queue_start_pending = None;
                    self.clear_play_pending();
                    self.queued_play = None;
                    self.toast(self.localize.t(
                        self.locale,
                        "Choose a device, or enable playback on this computer",
                    ));
                    self.show_devices = true;
                    self.refresh_devices();
                }
            }
        }
    }

    /// A playlist's disk cache has been read. Whether or not the page
    /// still matches it, what the Web API said about each song's
    /// availability holds, for rows already shown and rows to come.
    pub fn receive_playlist_cache(
        &mut self,
        account_id: &str,
        id: &str,
        generation: u64,
        mut cache: Option<PlaylistCache>,
    ) {
        if self.user_id() != Some(account_id) {
            return;
        }
        if let Some(page) = self.playlist_pages.get_mut(id) {
            if page.generation != generation {
                return;
            }
            page.cache_checked = true;
            if let Some(cache) = &mut cache {
                for (uri, playable) in known_availability(&cache.items) {
                    self.playlist_availability
                        .entry(uri.to_string())
                        .or_insert(playable);
                }
                // A cache may arrive after a fresh answer from another page.
                // Correct its flags before this prefix can be adopted.
                for row in &mut cache.items {
                    if let Some(PlayableItem::Track(track)) = row.item.as_mut()
                        && let Some(playable) = self.playlist_availability.get(&track.uri)
                    {
                        if track.is_playable != Some(*playable) {
                            cache.appendable = false;
                        }
                        track.is_playable = Some(*playable);
                    }
                }
                if fill_availability(&self.playlist_availability, &mut page.items.items) {
                    page.items.revision = page.items.revision.wrapping_add(1);
                    page.cache_append_valid = false;
                }
            }
            page.pending_cache = cache;
        }
        self.try_adopt_playlist_cache(id);
        self.checkpoint_playlist_cache(id);
    }

    pub fn receive_playlist_cache_stored(
        &mut self,
        account_id: &str,
        id: &str,
        generation: u64,
        snapshot: &str,
        success: bool,
    ) {
        self.playlist_cache_write_in_flight = false;
        let same_account = self.user_id() == Some(account_id);
        let should_check_again = same_account
            && self.playlist_pages.get_mut(id).is_some_and(|page| {
                let Some(pending) = page.cache_write_pending.take() else {
                    return false;
                };
                if pending.generation != generation || pending.snapshot != snapshot {
                    page.cache_write_pending = Some(pending);
                    return false;
                }
                let current = page.generation == generation
                    && page.items_generation == generation
                    && page.pending_writes == 0
                    && page
                        .playlist
                        .get()
                        .and_then(|playlist| playlist.snapshot_id.as_deref())
                        == Some(snapshot);
                if !current {
                    page.cache_append_valid = false;
                    return true;
                }
                if success && page.cache_append_valid {
                    page.cache_saved_through = Some(pending.through);
                    page.cache_saved_rows = pending.rows;
                    page.cache_saved_total = Some(pending.total);
                } else if !success {
                    page.cache_append_valid = false;
                }
                success || !pending.replacing
            });
        if should_check_again {
            self.checkpoint_playlist_cache(id);
        }
        if !self.playlist_cache_write_in_flight {
            let waiting: Vec<_> = self.playlist_pages
                .keys()
                .filter(|waiting| !same_account || waiting.as_str() != id)
                .cloned()
                .collect();
            for waiting in waiting {
                self.checkpoint_playlist_cache(&waiting);
                if self.playlist_cache_write_in_flight {
                    break;
                }
            }
        }
    }

    /// Adopt a playlist's cached prefix once Spotify confirms its snapshot.
    pub fn try_adopt_playlist_cache(&mut self, id: &str) {
        let mut uris = Vec::new();
        let mut adders: Vec<String> = Vec::new();
        let mut tracks = Vec::new();
        if let Some(page) = self.playlist_pages.get_mut(id) {
            if page.pending_writes > 0 {
                return;
            }
            let Some(snapshot_now) = page
                .playlist
                .get()
                .and_then(|playlist| playlist.snapshot_id.clone())
            else {
                return;
            };
            let confirmed_total = page.playlist.get().and_then(|playlist| {
                playlist
                    .items_count
                    .as_ref()
                    .or(playlist.tracks.as_ref())
                    .map(|count| count.total)
            });
            match &page.pending_cache {
                Some(cache)
                    if cache.snapshot == snapshot_now
                        && confirmed_total.is_none_or(|total| cache.total == total) => {}
                Some(_) => {
                    // A revision alone cannot validate an inconsistent cache.
                    // Let the live item request establish the rows and order.
                    page.pending_cache = None;
                    return;
                }
                None => return,
            }
            let Some(cache) = page.pending_cache.take() else {
                return;
            };
            let cached_through = cache.next_offset.unwrap_or(cache.total);
            let cached_rows = cache.items.len();
            let cached_total = cache.total;
            let cache_appendable = cache.appendable;
            let loaded_through = if page.items.base_offset == 0 {
                page.items.items.len() as u32
            } else {
                page.items
                    .windows
                    .get(&0)
                    .map_or(0, |items| items.len() as u32)
            };
            if loaded_through >= cached_through {
                page.cache_saved_through = Some(cached_through);
                page.cache_saved_rows = cached_rows;
                page.cache_saved_total = Some(cached_total);
                page.cache_append_valid = false;
                page.pending_cache = None;
                return;
            }
            uris = cache
                .items
                .iter()
                .filter_map(|item| item.playable())
                .map(|item| item.uri().to_string())
                .collect();
            tracks = cache
                .items
                .iter()
                .filter_map(|item| match item.playable() {
                    Some(PlayableItem::Track(track)) => Some(track.clone()),
                    _ => None,
                })
                .collect();
            adders = cache
                .items
                .iter()
                .filter_map(|item| item.added_by.as_ref()?.id.clone())
                .filter(|id| !id.is_empty())
                .collect();
            page.contributors.extend(adders.iter().cloned());
            page.items
                .adopt_cached_prefix(cache.items, cache.total, cache.next_offset);
            page.items_generation = page.generation;
            page.cache_saved_through = Some(cached_through);
            page.cache_saved_rows = cached_rows;
            page.cache_saved_total = Some(cached_total);
            page.cache_append_valid = cache_appendable;
            page.cache_restored_through = Some(cached_through);
        }
        for track in &tracks {
            self.remember_track_recording(track);
        }
        self.request_contains(uris);
        self.request_user_names(adders);
        self.sample_playlist_tail(id);
        if self.table_sorts
            .contains_key(&Page::Playlist(id.to_string()))
        {
            self.load_more(Page::Playlist(id.to_string()));
        }
    }

    /// Read the final page once for collaborators when the loaded rows do not
    /// already include it. This also runs after a disk cache wins startup.
    pub fn sample_playlist_tail(&mut self, id: &str) {
        let request = self.playlist_pages.get_mut(id).and_then(|page| {
            if page.tail_checked || !page.items.loaded_once {
                return None;
            }
            page.tail_checked = true;
            let loaded_end = page
                .items
                .base_offset
                .saturating_add(page.items.items.len().try_into().unwrap_or(u32::MAX));
            let total = page.items.total.filter(|total| *total > loaded_end)?;
            Some(ApiRequest::PlaylistSample {
                id: id.to_string(),
                offset: total.saturating_sub(PLAYLIST_PAGE_SIZE),
                generation: page.generation,
            })
        });
        if let Some(request) = request {
            self.backend.api(request);
        }
    }

    /// Save the first page, every ten pages after that, and the completed list.
    /// Only rows added since the confirmed checkpoint are copied on appends.
    pub fn checkpoint_playlist_cache(&mut self, id: &str) {
        const CHECKPOINT_ITEMS: u32 = PLAYLIST_PAGE_SIZE * 10;

        if self.playlist_cache_write_in_flight {
            return;
        }
        let command = self.playlist_pages.get_mut(id).and_then(|page| {
            let snapshot = page
                .playlist
                .get()
                .and_then(|playlist| playlist.snapshot_id.clone())?;
            if !page.items.loaded_once
                || page.items.items.is_empty()
                || page.items.base_offset != 0
                || page.items_generation != page.generation
                || page.pending_writes > 0
                || page.cache_write_pending.is_some()
            {
                return None;
            }
            if !page.cache_checked {
                return None;
            }
            let total = page.items.total?;
            let next_offset = page.items.next_offset;
            let through = next_offset.unwrap_or(total);
            let previous = page.cache_saved_through.unwrap_or(0);
            let complete = next_offset.is_none();
            let row_count = page.items.items.len();
            if page.cache_append_valid
                && page.cache_saved_through == Some(through)
                && page.cache_saved_rows == row_count
                && page.cache_saved_total == Some(total)
            {
                return None;
            }
            if previous > 0 && through.saturating_sub(previous) < CHECKPOINT_ITEMS && !complete {
                return None;
            }
            let replacing = !page.cache_append_valid
                || page.cache_saved_through.is_none()
                || page.cache_saved_total != Some(total)
                || page.cache_saved_rows > row_count;
            let rows = if replacing {
                PlaylistCacheRows::Replace(page.items.items.clone())
            } else {
                PlaylistCacheRows::Append {
                    previous_rows: page.cache_saved_rows,
                    previous_offset: previous,
                    items: page.items.items[page.cache_saved_rows..].to_vec(),
                }
            };
            page.cache_write_pending = Some(PlaylistCachePending {
                generation: page.generation,
                snapshot: snapshot.clone(),
                through,
                rows: row_count,
                total,
                replacing,
            });
            if replacing {
                page.cache_append_valid = true;
            }
            Some(Command::StorePlaylistCache {
                id: id.to_string(),
                generation: page.generation,
                snapshot,
                rows,
                total,
                next_offset,
            })
        });
        if let Some(command) = command {
            self.playlist_cache_write_in_flight = true;
            self.backend.send(command);
        }
    }

    /// Play what was playing when the app last closed. `false` when
    /// nothing is known to resume.
    pub fn resume_last(&mut self) -> bool {
        let Some(track) = self.resume_track.clone() else {
            return false;
        };
        let mut request = match self.resume_context.clone() {
            Some(context) => PlayRequest::context(context).starting_at_uri(track),
            None => PlayRequest::tracks(vec![track]),
        };
        request.position_ms = self.resume_position_ms;
        self.play_request(request, false);
        true
    }

    pub fn toggle_play(&mut self) {
        let playing = self.now_playing().map(|now| now.playing);
        match self.target() {
            Target::Local => {
                if self.local.is_active() {
                    self.backend.player(PlayerCommand::Toggle);
                } else if let Some(remote) = self.remote_fresh() {
                    // Nothing is playing locally: resume on this computer.
                    let uri = remote
                        .state
                        .item
                        .as_ref()
                        .map(|item| item.uri().to_string());
                    let position = remote.state.progress_ms.unwrap_or(0);
                    if let Some(uri) = uri {
                        let mut request = match &remote.state.context {
                            Some(context) if !context.uri.is_empty() => {
                                PlayRequest::context(context.uri.clone()).starting_at_uri(uri)
                            }
                            _ => PlayRequest::tracks(vec![uri]),
                        };
                        request.position_ms = position;
                        self.play_request(request, false);
                        return;
                    }
                    if !self.resume_last() {
                        self.toast(self.localize.t(self.locale, "Pick something to play"));
                    }
                    return;
                } else {
                    if !self.resume_last() {
                        self.toast(self.localize.t(self.locale, "Pick something to play"));
                    }
                    return;
                }
            }
            Target::Remote(device_id) => {
                if device_id.is_none() && self.remote_fresh().is_none() {
                    // Nothing is known to be playing anywhere, which is how
                    // a fresh start looks before the local engine is ready:
                    // pick up where the last run left off, the way the
                    // local branch does. The engine plays it once it is up.
                    if !self.resume_last() {
                        self.toast(self.localize.t(self.locale, "Pick a song, album, or playlist"));
                    }
                    return;
                }
                self.set_play_pending(vec!["::toggle".into()]);
                if playing == Some(true) {
                    self.remote(RemoteAction::Pause, device_id);
                } else {
                    self.remote(RemoteAction::Play, device_id);
                }
            }
        }
        if let Some(playing) = playing {
            self.optimistic_playing = Some((!playing, Instant::now()));
        }
    }

    pub fn seek(&mut self, position_ms: u32) {
        // Dragging the bar under the remembered song moves the point a press
        // of play will resume from; there is no stream to seek yet.
        if self.now_playing_live().is_none() && self.resume_track.is_some() {
            self.resume_position_ms = position_ms;
            self.session_dirty = true;
            return;
        }
        match self.target() {
            Target::Local => {
                self.pending_local_position = Some((position_ms, Instant::now()));
                self.backend.player(PlayerCommand::Seek(position_ms));
            }
            Target::Remote(device_id) => {
                self.pending_remote_position = Some((position_ms, Instant::now()));
                self.backend.api(ApiRequest::Remote {
                    action: RemoteAction::Seek,
                    device_id,
                    play: None,
                    position_ms,
                    percent: 0,
                    flag: false,
                    repeat: String::new(),
                });
            }
        }
    }

    /// The volume this side set that the engine has yet to confirm, if the
    /// hold is still good. Clears itself once the engine agrees or it expires.
    pub fn held_local_volume(&mut self, reported: u16) -> Option<u16> {
        match self.pending_local_volume {
            Some((volume, at)) if volume != reported && at.elapsed() < OPTIMISTIC_HOLD => {
                Some(volume)
            }
            _ => {
                self.pending_local_volume = None;
                None
            }
        }
    }

    /// `settle` is false while the slider is still moving: the level is heard
    /// at once, and Spotify is told where it ended up on release.
    /// Whether the playing device takes volume changes from Spotifast.
    /// Spotify refuses them for some remote devices, so the controls are
    /// disabled for those rather than failing when used.
    pub fn can_set_volume(&self) -> bool {
        self.now_playing().is_none_or(|now| now.can_set_volume)
    }

    pub fn set_volume(&mut self, percent: u8, settle: bool) {
        let percent = percent.min(100);
        match self.target() {
            Target::Local => {
                let volume = percent_to_volume(percent);
                self.local.volume = volume;
                self.pending_local_volume = Some((volume, Instant::now()));
                // The engine echoes `VolumeChanged` only while this device
                // holds the Connect session, so the snapshot that would
                // otherwise persist this may never arrive.
                if self.settings.volume != volume {
                    self.settings.volume = volume;
                    self.settings_dirty = true;
                }
                self.backend.player(if settle {
                    PlayerCommand::Volume(volume)
                } else {
                    PlayerCommand::VolumePreview(volume)
                });
            }
            Target::Remote(_) if !settle || !self.can_set_volume() => {}
            Target::Remote(device_id) => {
                self.pending_remote_volume = Some((percent, Instant::now()));
                self.backend.api(ApiRequest::Remote {
                    action: RemoteAction::Volume,
                    device_id,
                    play: None,
                    position_ms: 0,
                    percent,
                    flag: false,
                    repeat: String::new(),
                });
            }
        }
    }

    pub fn note_shuffle_pending(&mut self) {
        if let Loadable::Loaded(queue) = &self.queue {
            let at = Self::end_of_queued_rows(&queue.queue, &self.manual_queue);
            let context_uris: Vec<String> = queue.queue[at..]
                .iter()
                .map(|item| item.uri().to_string())
                .collect();
            if context_uris.len() > 1 && context_uris.iter().any(|uri| uri != &context_uris[0]) {
                self.queue_shuffle_pending = Some(QueueShufflePending {
                    current_uri: self.current_track_uri(),
                    context_uris,
                    at: Instant::now(),
                });
            } else {
                self.queue_shuffle_pending = None;
            }
        }
    }

    pub fn set_shuffle(&mut self, shuffle: bool) {
        self.shuffle_wanted = shuffle;
        self.shuffle_set_at = Some(Instant::now());
        self.session_dirty = true;
        if let Some(assumed) = &mut self.assumed_context {
            assumed.shuffle = Some(shuffle);
        }
        self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
        self.note_shuffle_pending();
        match self.target() {
            Target::Local => {
                self.local.shuffle = shuffle;
                self.backend.player(PlayerCommand::Shuffle(shuffle));
            }
            Target::Remote(None) if self.remote_fresh().is_none() => {
                // Keep the mode for Play without requesting an absent device.
            }
            Target::Remote(device_id) => {
                if let Some(remote) = self.remote.as_mut() {
                    remote.state.shuffle_state = shuffle;
                }
                self.backend.api(ApiRequest::Remote {
                    action: RemoteAction::Shuffle,
                    device_id,
                    play: None,
                    position_ms: 0,
                    percent: 0,
                    flag: shuffle,
                    repeat: String::new(),
                });
            }
        }
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        match self.target() {
            Target::Local => {
                self.local.repeat = mode;
                self.backend.player(PlayerCommand::Repeat(mode));
            }
            Target::Remote(device_id) => {
                if let Some(remote) = self.remote.as_mut() {
                    remote.state.repeat_state = mode.api_name().to_string();
                }
                self.backend.api(ApiRequest::Remote {
                    action: RemoteAction::Repeat,
                    device_id,
                    play: None,
                    position_ms: 0,
                    percent: 0,
                    flag: false,
                    repeat: mode.api_name().to_string(),
                });
            }
        }
    }

    pub fn transfer(&mut self, device_id: String) {
        if Some(device_id.as_str()) == self.local_device_id.as_deref() {
            self.selected_device = None;
            self.show_devices = false;
            if !self.local.is_active() {
                // Connect transfers the active device's full playback state.
                // A Web API snapshot may be stale and cannot recreate its queue.
                self.queue_start_pending = Some(Target::Local);
                self.local_transfer_sequence = Some(self.local.track_sequence);
                self.local_list = None;
                self.resume_queue.clear();
                self.queued_play = None;
                self.intent_track = None;
                self.assumed_context = None;
                self.optimistic_playing = None;
                self.clear_play_pending();
                self.backend.player(PlayerCommand::Transfer);
            }
            self.poll_remote_soon();
            return;
        }
        let play = self.now_playing().is_some_and(|now| now.playing);
        self.transfer_remote(device_id, play);
    }

    pub fn transfer_remote(&mut self, device_id: String, play: bool) {
        self.selected_device = Some(device_id.clone());
        self.backend.api(ApiRequest::Transfer { device_id, play });
    }

    /// Adds a row to Next up immediately, before the context's upcoming rows.
    pub fn add_to_queue(&mut self, uri: String, label: String) {
        if util::uri_kind(&uri) == Some("album") {
            self.queue_album(uri, label);
            return;
        }
        if self.queued_moments_ago(&uri) {
            return;
        }
        self.queue_one(uri, label, true);
    }

    /// Whether `uri` was queued so recently that asking again is the same
    /// click arriving twice. Later duplicates are separate asks.
    pub fn queued_moments_ago(&mut self, uri: &str) -> bool {
        self.expire_pending_queue_adds();
        self.pending_queue_adds
            .iter()
            .any(|pending| pending.item.uri() == uri && pending.at.elapsed() < QUEUE_ADD_DEBOUNCE)
    }

    pub fn queue_album(&mut self, uri: String, label: String) {
        if self.last_album_queue
            .as_ref()
            .is_some_and(|(previous, at)| previous == &uri && at.elapsed() < QUEUE_ADD_DEBOUNCE)
        {
            return;
        }
        let Some(id) = util::uri_id(&uri).map(str::to_string) else {
            return;
        };
        self.last_album_queue = Some((uri, Instant::now()));
        if let Some(page) = self.album_pages.get(&id)
            && page.tracks.is_complete()
        {
            self.queue_album_tracks(page.tracks.items.clone(), label);
            return;
        }
        self.album_queue_serial = self.album_queue_serial.wrapping_add(1);
        let request = self.album_queue_serial;
        self.pending_album_queues.insert(
            request,
            PendingAlbumQueue {
                id: id.clone(),
                label: label.clone(),
                target: self.target(),
                offset: 0,
                tracks: Vec::new(),
            },
        );
        self.backend.api(ApiRequest::AlbumQueueTracks {
            id,
            offset: 0,
            request,
        });
        // Translators: {name} is an album name.
        self.toast(self.localize.t(self.locale, "Loading {name} to queue…").replace("{name}", &label));
    }

    pub fn receive_album_queue(
        &mut self,
        request: u64,
        offset: u32,
        result: crate::api::client::Result<crate::api::models::Page<Track>>,
    ) {
        let Some(pending) = self.pending_album_queues.get(&request) else {
            return;
        };
        if pending.offset != offset {
            return;
        }
        let mut pending = self.pending_album_queues.remove(&request).unwrap();
        if pending.target != self.target() {
            self.toast_error(self.localize.t(
                self.locale,
                "Playback device changed. Add the album to queue again",
            ));
            return;
        }
        let page = match result {
            Ok(page) if page.offset == offset => page,
            Ok(_) => {
                self.toast_error(self.localize.t(
                    self.locale,
                    "Couldn't load the album's songs in order. Try again",
                ));
                return;
            }
            Err(error) => {
                self.toast_error(
                    // Translators: {name} is an album name and {error} is an error message.
                    self.localize.t(self.locale, "Couldn't add {name} to queue: {error}")
                        .replace("{name}", &pending.label)
                        .replace("{error}", &error.to_string()),
                );
                return;
            }
        };
        let next = page.next_offset();
        if page.next.is_some() && next.is_none() {
            self.toast_error(self.localize.t(
                self.locale,
                "Couldn't load the album's songs in order. Try again",
            ));
            return;
        }
        pending.tracks.extend(page.items);
        if let Some(next) = next {
            if next <= offset {
                self.toast_error(self.localize.t(
                    self.locale,
                    "Couldn't load the album's songs in order. Try again",
                ));
                return;
            }
            pending.offset = next;
            self.backend.api(ApiRequest::AlbumQueueTracks {
                id: pending.id.clone(),
                offset: next,
                request,
            });
            self.pending_album_queues.insert(request, pending);
        } else {
            self.queue_album_tracks(pending.tracks, pending.label);
        }
    }

    pub fn queue_album_tracks(&mut self, tracks: Vec<Track>, label: String) {
        let pending_start = self.pending_queue_adds.len();
        let mut uris = Vec::new();
        for track in tracks {
            if track.is_local
                || track.is_playable == Some(false)
                || util::uri_kind(&track.uri) != Some("track")
            {
                continue;
            }
            let uri = track.uri.clone();
            let name = track.name.clone();
            if let Some(id) = util::uri_id(&uri) {
                self.remember_track_recording(&track);
                self.track_cache.insert(id.to_string(), track);
            }
            self.show_queued_song(&uri, &name);
            uris.push(uri);
        }
        if uris.is_empty() {
            self.toast_error(self.localize.t(
                self.locale,
                "This album has no playable songs to queue",
            ));
            return;
        }
        let count = uris.len();
        self.toast(
            self.localize.tn(
                self.locale,
                // Translators: {count} is a number of songs and {name} is an album name.
                "{count} song from {name} added to queue",
                "{count} songs from {name} added to queue",
                count as u32,
            )
            .replace("{count}", &count.to_string())
            .replace("{name}", &label),
        );
        if self.local.is_active() && self.target() == Target::Local {
            for uri in uris {
                self.backend.player(PlayerCommand::AddToQueue(uri));
            }
            self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
        } else {
            self.write_queue_adds(uris, pending_start);
        }
    }

    /// Queues several songs after the ones already queued, skipping any
    /// queued again within `QUEUE_ADD_DEBOUNCE`, with one combined toast.
    pub fn queue_many(&mut self, songs: Vec<(String, String)>) {
        // Each picked row is its own ask, so a song picked twice is queued
        // twice. Only an add from an earlier click can make one of them a
        // repeat, so decide that before adding any.
        let repeats: Vec<bool> = songs
            .iter()
            .map(|(uri, _)| self.queued_moments_ago(uri))
            .collect();
        let mut count = 0;
        for ((uri, label), repeat) in songs.into_iter().zip(repeats) {
            if !repeat {
                self.queue_one(uri, label, false);
                count += 1;
            }
        }
        if count > 0 {
            self.queued_toast(count);
        }
    }

    pub fn queued_toast(&mut self, count: usize) {
        self.toast(
            self.localize.tn(
                self.locale,
                // Translators: {count} is a number of songs.
                "{count} song added to queue",
                "{count} songs added to queue",
                count as u32,
            )
            .replace("{count}", &count.to_string()),
        );
    }

    /// Replays the manually queued songs on the local engine in their
    /// current order. librespot can only append to or clear a live queue,
    /// so a move or positional insert clears it and re-adds every song.
    pub fn resync_local_queue(&mut self) {
        self.backend.player(PlayerCommand::ClearQueue);
        for uri in self.manual_queue.clone() {
            if uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:") {
                self.backend.player(PlayerCommand::AddToQueue(uri));
            }
        }
        // Drop any queue fetch already in flight: it was asked for before
        // the move and would otherwise land with the pre-move order. Each
        // change gets the full allowance of stale answers: the engine
        // reports every step of the clear and re-add, and a budget spent on
        // an earlier move would let one of those half-done answers through.
        self.queue_seq += 1;
        self.queue_stale_retries = 0;
        self.queue_reorder_pending = Some(Instant::now());
        self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
    }

    /// Adds one song after existing manual queue entries.
    ///
    /// `announce` is false when a batch should produce one toast.
    pub fn queue_one(&mut self, uri: String, label: String, announce: bool) {
        let pending_start = self.pending_queue_adds.len();
        self.show_queued_song(&uri, &label);
        if announce {
            // Translators: {name} is a song, episode, album, or playlist name.
            self.toast(self.localize.t(self.locale, "{name} added to queue").replace("{name}", &label));
        }
        // Queue tracks and episodes directly on the active local engine.
        // Other targets and item types use the Web API.
        let track_like = uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:");
        if track_like && self.local.is_active() && matches!(self.target(), Target::Local) {
            self.backend.player(PlayerCommand::AddToQueue(uri));
            self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
            return;
        }
        self.write_queue_adds(vec![uri], pending_start);
    }

    pub fn show_queued_song(&mut self, uri: &str, label: &str) {
        let item = self.optimistic_queue_item(uri, label);
        self.pending_queue_adds.push(PendingQueueAdd {
            item: item.clone(),
            at: Instant::now(),
            manual_index: self.manual_queue.len(),
            write: None,
        });
        if let Loadable::Loaded(queue) = &self.queue {
            let at = Self::end_of_queued_rows(&queue.queue, &self.manual_queue);
            if let Loadable::Loaded(queue) = &mut self.queue {
                queue.queue.insert(at, item);
            }
        }
        self.manual_queue.push(uri.to_string());
        self.session_dirty = true;
    }

    pub fn write_queue_adds(&mut self, uris: Vec<String>, pending_start: usize) {
        let target = self.target();
        let device_id = match &target {
            Target::Local => self.local_device_id.clone(),
            Target::Remote(device_id) => device_id.clone(),
        };
        self.album_queue_serial = self.album_queue_serial.wrapping_add(1);
        let request = self.album_queue_serial;
        for (index, addition) in self.pending_queue_adds[pending_start..]
            .iter_mut()
            .enumerate()
        {
            addition.write = Some((request, index));
        }
        self.pending_queue_batches.insert(request, target);
        self.backend.api(ApiRequest::AddManyToQueue {
            request,
            uris,
            device_id,
        });
    }

    /// Roll back only the rejected occurrences, leaving accepted songs,
    /// later additions and matching songs in the playing context intact.
    pub fn remove_failed_queue_adds(&mut self, request: u64, added: usize) {
        let mut indexes: Vec<_> = self.pending_queue_adds
            .iter()
            .filter(|addition| {
                addition
                    .write
                    .is_some_and(|(write, index)| write == request && index >= added)
            })
            .map(|addition| addition.manual_index)
            .collect();
        indexes.sort_unstable();
        for index in indexes.into_iter().rev() {
            let Some(uri) = self.manual_queue.get(index) else {
                continue;
            };
            let occurrence = self.manual_queue[..index]
                .iter()
                .filter(|other| *other == uri)
                .count();
            if let Loadable::Loaded(queue) = &mut self.queue {
                let queued_len = Self::end_of_queued_rows(&queue.queue, &self.manual_queue);
                if let Some(row) = queue.queue[..queued_len]
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.uri() == uri)
                    .nth(occurrence)
                    .map(|(row, _)| row)
                {
                    queue.queue.remove(row);
                }
            }
            self.remove_manual_queue_row(index);
        }
    }

    /// Puts the first `queued_len` rows on show, the Playing next section,
    /// in `manual_queue` order, which is the order the engine is given. A
    /// move works on `manual_queue`; when an accepted answer from Spotify
    /// had those rows in another order, moving the shown row at the same
    /// index would move a different song than the engine does.
    pub fn show_manual_queue_order(&mut self, queued_len: usize) {
        let Loadable::Loaded(queue) = &mut self.queue else {
            return;
        };
        let queued_len = queued_len.min(queue.queue.len());
        let mut shown: Vec<Option<PlayableItem>> =
            queue.queue.drain(..queued_len).map(Some).collect();
        let mut ordered = Vec::with_capacity(shown.len());
        for uri in &self.manual_queue {
            if let Some(item) = shown
                .iter_mut()
                .find(|item| item.as_ref().is_some_and(|item| item.uri() == uri))
                .and_then(Option::take)
            {
                ordered.push(item);
            }
        }
        ordered.extend(shown.into_iter().flatten());
        queue.queue.splice(0..0, ordered);
    }

    /// Index after manual queue rows and before context rows.
    pub fn end_of_queued_rows(rows: &[PlayableItem], manual: &[String]) -> usize {
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for uri in manual {
            *counts.entry(uri.as_str()).or_insert(0) += 1;
        }
        let mut at = 0;
        for item in rows {
            match counts.get_mut(item.uri()) {
                Some(left) if *left > 0 => {
                    *left -= 1;
                    at += 1;
                }
                _ => break,
            }
        }
        at
    }

    /// Queue row built from cached details or a temporary label.
    pub fn optimistic_queue_item(&self, uri: &str, label: &str) -> PlayableItem {
        let cached = util::uri_id(uri)
            .and_then(|id| self.track_cache.get(id))
            .cloned();
        if let Some(track) = cached {
            return PlayableItem::Track(track);
        }
        if let Some(item) = self.queue.get().and_then(|queue| {
            queue
                .currently_playing
                .iter()
                .chain(&queue.queue)
                .find(|item| item.uri() == uri)
        }) {
            return item.clone();
        }
        if let Some(item) = self.playlist_pages
            .values()
            .flat_map(|page| &page.items.items)
            .filter_map(PlaylistItem::playable)
            .find(|item| item.uri() == uri)
        {
            return item.clone();
        }
        let known = self.album_pages
            .values()
            .flat_map(|page| &page.tracks.items)
            .chain(self.library.liked.items.iter().map(|saved| &saved.track))
            .chain(
                self.search
                    .results
                    .get()
                    .into_iter()
                    .flat_map(|results| results.tracks.iter().flat_map(|page| &page.items)),
            )
            .find(|track| track.uri == uri);
        if let Some(track) = known {
            return PlayableItem::Track(track.clone());
        }
        PlayableItem::Track(crate::api::models::Track {
            uri: uri.to_string(),
            name: label.to_string(),
            ..Default::default()
        })
    }

    pub fn expire_pending_queue_adds(&mut self) {
        if !self.pending_queue_batches.is_empty() {
            return;
        }
        self.pending_queue_adds
            .retain(|addition| addition.at.elapsed() < Duration::from_secs(30));
    }

    /// Restores pending additions missing from a stale fetched queue.
    pub fn reconcile_pending_queue(&mut self) {
        self.expire_pending_queue_adds();
        if self.pending_queue_adds.is_empty() {
            return;
        }
        let Loadable::Loaded(queue) = &mut self.queue else {
            return;
        };
        let fetched: std::collections::HashSet<String> = queue
            .queue
            .iter()
            .map(|item| item.uri().to_string())
            .collect();
        // A fetched or currently playing addition is no longer pending.
        let current = self.current_track_uri();
        self.pending_queue_adds.retain(|addition| {
            !fetched.contains(addition.item.uri())
                && current.as_deref() != Some(addition.item.uri())
        });
        let missing: Vec<PlayableItem> = self.pending_queue_adds
            .iter()
            .map(|addition| addition.item.clone())
            .collect();
        let at = match &self.queue {
            Loadable::Loaded(queue) => Self::end_of_queued_rows(&queue.queue, &self.manual_queue),
            _ => 0,
        };
        if let Loadable::Loaded(queue) = &mut self.queue {
            for item in missing.into_iter().rev() {
                queue.queue.insert(at, item);
            }
        }
    }

    /// Answer from the rows already held locally. A found duplicate is
    /// definitive even for a partial playlist; an empty answer is definitive
    /// only when the whole playlist is present.
    pub fn local_playlist_duplicates(&self, id: &str, items: &[PlayableItem]) -> Option<Vec<String>> {
        let page = self.playlist_pages.get(id)?;
        let existing: HashSet<&str> = page
            .items
            .items
            .iter()
            .filter_map(PlaylistItem::playable)
            .map(PlayableItem::uri)
            .chain(page.local_additions.iter().map(String::as_str))
            .collect();
        let duplicates: Vec<String> = items
            .iter()
            .map(PlayableItem::uri)
            .filter(|uri| existing.contains(*uri))
            .map(str::to_string)
            .collect();
        if !duplicates.is_empty() || page.items.is_complete() {
            Some(duplicates)
        } else {
            None
        }
    }

    pub fn prepare_playlist_mutation(&mut self, id: &str) {
        let Some(page) = self.playlist_pages.get_mut(id) else {
            return;
        };
        page.pending_writes += 1;
        self.load_generation += 1;
        page.generation = self.load_generation;
        page.items_generation = page.generation;
        // Reads issued before the edit describe the old snapshot and must not
        // be allowed to replace the optimistic rows when they arrive.
        page.items.loading = page.refresh_after_write;
        page.items.clear_windows();
        page.cache_saved_through = None;
        page.cache_saved_rows = 0;
        page.cache_saved_total = None;
        // Let the old write finish before checkpointing the edited snapshot.
        page.cache_append_valid = false;
        page.cache_restored_through = None;
        page.pending_cache = None;
    }

    pub fn request_playlist_add(
        &mut self,
        playlist_id: String,
        playlist_name: String,
        items: Vec<PlayableItem>,
        position: Option<u32>,
    ) {
        match self.local_playlist_duplicates(&playlist_id, &items) {
            Some(duplicate_uris) if duplicate_uris.is_empty() => {
                self.add_to_playlist_now(playlist_id, playlist_name, items, position);
            }
            Some(duplicate_uris) => {
                self.dialog = Some(Dialog::ConfirmPlaylistDuplicates {
                    playlist_id,
                    playlist_name,
                    items,
                    position,
                    duplicate_uris,
                });
            }
            None => {
                self.playlist_busy = true;
                self.backend.api(ApiRequest::CheckPlaylistDuplicates {
                    playlist_id,
                    playlist_name,
                    items,
                    position,
                });
            }
        }
    }

    pub fn add_to_playlist_now(
        &mut self,
        playlist_id: String,
        playlist_name: String,
        items: Vec<PlayableItem>,
        position: Option<u32>,
    ) {
        if items.is_empty() {
            return;
        }
        self.dialog = None;
        self.playlist_busy = true;
        for item in &items {
            if let PlayableItem::Track(track) = item {
                self.remember_track_recording(track);
            }
        }

        self.prepare_playlist_mutation(&playlist_id);
        let added = items.len().try_into().unwrap_or(u32::MAX);
        let added_by = self.user_id().map(|id| UserRef {
            id: Some(id.to_string()),
        });
        let added_at = Some(jiff::Timestamp::now().to_string());
        if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
            let current_total = page.items.total.unwrap_or_else(|| {
                page.items
                    .base_offset
                    .saturating_add(page.items.items.len() as u32)
            });
            let at = position.unwrap_or(current_total);
            let start = page.items.base_offset;
            let end = start.saturating_add(page.items.items.len() as u32);
            for item in &items {
                page.local_additions.insert(item.uri().to_string());
            }
            if page.items.loaded_once && (start..=end).contains(&at) {
                let relative = (at - start) as usize;
                page.items.items.splice(
                    relative..relative,
                    items.iter().cloned().map(|item| PlaylistItem {
                        added_at: added_at.clone(),
                        added_by: added_by.clone(),
                        is_local: false,
                        item: Some(item),
                        track: None,
                    }),
                );
            } else if at < start {
                // A delayed confirmation can arrive after the user opened a
                // different slice. Keep that slice attached to the same rows.
                page.items.base_offset = start.saturating_add(added);
            }
            if page.items.loaded_once
                && at <= end
                && let Some(next) = &mut page.items.next_offset
            {
                *next = next.saturating_add(added);
            }
            page.items.total = Some(current_total.saturating_add(added));
            page.items.revision = page.items.revision.wrapping_add(1);
            if let Some(playlist) = page.playlist.get_mut() {
                increase_playlist_total(playlist, added);
            }
        }
        if let Some(playlists) = self.library.playlists.get_mut()
            && let Some(playlist) = playlists
                .iter_mut()
                .find(|playlist| playlist.id == playlist_id)
        {
            increase_playlist_total(playlist, added);
        }

        self.backend.api(ApiRequest::AddToPlaylist {
            playlist_id,
            playlist_name,
            uris: items.iter().map(|item| item.uri().to_string()).collect(),
            position,
        });
    }

    /// Appends the songs linked in pasted text to an editable playlist.
    /// Songs this app already knows add their rows at once; the rest are
    /// asked of Spotify first, so that every row has its name.
    pub fn paste_songs(&mut self, playlist_id: String, text: &str) {
        let Some(playlist_name) = self.playlist_pages
            .get(&playlist_id)
            .and_then(|page| page.playlist.get())
            .filter(|playlist| self.can_edit_playlist(playlist))
            .map(|playlist| playlist.name.clone())
        else {
            return;
        };
        let uris: Vec<String> = text
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter_map(crate::link::parse)
            .filter(|uri| matches!(util::uri_kind(uri), Some("track" | "episode")))
            .collect();
        if uris.is_empty() {
            self.toast(self.localize.t(
                self.locale,
                "The clipboard has no Spotify song links",
            ));
            return;
        }
        let mut paste = PendingPaste {
            playlist_id,
            playlist_name,
            uris,
            found: HashMap::new(),
            missing: HashSet::new(),
        };
        for uri in paste.uris.clone() {
            if paste.found.contains_key(&uri) || paste.missing.contains(&uri) {
                continue;
            }
            if let Some(item) = self.known_song(&uri) {
                paste.found.insert(uri, item);
            } else if let Some(id) = uri.strip_prefix("spotify:track:").map(str::to_string) {
                if self.track_requests.insert(id.clone()) {
                    self.backend.api(ApiRequest::Track { id });
                }
            } else {
                // Episodes are only added from what this app has shown.
                paste.missing.insert(uri);
            }
        }
        self.pending_pastes.push(paste);
        self.finish_pastes();
    }

    /// A song this app has already shown, by its URI.
    pub fn known_song(&mut self, uri: &str) -> Option<PlayableItem> {
        if let Some(item) = self.copied_songs.iter().find(|item| item.uri() == uri) {
            return Some(item.clone());
        }
        let id = uri.strip_prefix("spotify:track:")?;
        self.read_cached_track(id).map(PlayableItem::Track)
    }

    /// Records Spotify's answer for a pasted song link: the song, or `None`
    /// when there is no such song.
    pub fn resolve_pasted_song(&mut self, uri: &str, item: Option<PlayableItem>) {
        if self.pending_pastes.is_empty() {
            return;
        }
        for paste in &mut self.pending_pastes {
            if !paste.uris.iter().any(|pasted| pasted == uri) {
                continue;
            }
            match &item {
                Some(item) => {
                    paste.found.insert(uri.to_string(), item.clone());
                }
                None => {
                    paste.missing.insert(uri.to_string());
                }
            }
        }
        self.finish_pastes();
    }

    /// Adds every paste whose songs are all known, or known to be missing.
    pub fn finish_pastes(&mut self) {
        let mut index = 0;
        while index < self.pending_pastes.len() {
            if !self.pending_pastes[index].settled() {
                index += 1;
                continue;
            }
            let paste = self.pending_pastes.remove(index);
            let items: Vec<PlayableItem> = paste
                .uris
                .iter()
                .filter_map(|uri| paste.found.get(uri).cloned())
                .collect();
            let skipped = paste.uris.len() - items.len();
            if skipped > 0 {
                self.toast_error(
                    self.localize.tn(
                        self.locale,
                        // Translators: {count} is the number of pasted links that failed.
                        "{count} pasted link could not be added",
                        "{count} pasted links could not be added",
                        skipped as u32,
                    )
                    .replace("{count}", &skipped.to_string()),
                );
            }
            if !items.is_empty() {
                self.request_playlist_add(paste.playlist_id, paste.playlist_name, items, None);
            }
        }
    }

    pub fn set_saved(&mut self, uri: String, saved: bool) {
        if uri.starts_with("spotify:playlist:") {
            self.saved.insert(uri.clone(), saved);
            let id = util::uri_id(&uri).unwrap_or_default().to_string();
            self.backend
                .api(ApiRequest::FollowPlaylist { id, follow: saved });
            return;
        }
        self.set_saved_state(uri.clone(), saved);
        self.saved_writes.insert(uri.clone(), saved);
        if self.change_liked_song(&uri, saved) {
            self.sync_liked_songs();
            self.ensure_liked_songs();
        }
        self.backend.api(ApiRequest::SetSaved {
            uris: vec![uri],
            saved,
        });
    }

    // ---- actions -----------------------------------------------------------------





    /// Apply one queued UI action after drawing, updating optimistic local state
    /// and scheduling any backend work or preference persistence it requires.


    /// The details the edit dialog actually changed, compared with the
    /// playlist it was opened from. Spotify rejects an empty description
    /// with "Attribute description is empty", so clearing one sends nothing
    /// for it rather than failing the rename saved alongside. The dialog
    /// shows the description without its HTML, so an untouched one is not
    /// sent back stripped either.
    pub fn changed_playlist_details(
        &self,
        id: &str,
        name: String,
        description: String,
        public: Option<bool>,
    ) -> PlaylistDetailChanges {
        let original = self.playlist_pages
            .get(id)
            .and_then(|page| page.playlist.get())
            .or_else(|| {
                self.library
                    .playlists
                    .get()
                    .and_then(|playlists| playlists.iter().find(|playlist| playlist.id == id))
            });
        let original_description = original
            .and_then(|playlist| playlist.description.as_deref())
            .map(util::strip_html)
            .unwrap_or_default();
        let cleared = description.is_empty();
        PlaylistDetailChanges {
            name: original
                .is_none_or(|playlist| playlist.name != name)
                .then_some(name),
            kept_description: cleared && !original_description.is_empty(),
            description: (!cleared && description != original_description).then_some(description),
            public: public
                .filter(|public| original.is_none_or(|playlist| playlist.public != Some(*public))),
        }
    }

    pub fn report_update_failure(&mut self, error: String) {
        self.toast_error(error);
    }



    pub fn maybe_suggest_personal_app(&mut self) {
        if self.settings.personal_app_intro_seen
            || self.settings
                .web_client_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
            || self.web_app.is_some()
            || !self.is_connected()
            || self.offline
            || self.user.as_ref().and_then(|user| user.product.as_deref()) != Some("premium")
            || self.dialog.is_some()
            || self.show_devices
            || self.settings.winamp_window
            || self.page() == &Page::Settings
        {
            return;
        }
        self.dialog = Some(Dialog::PersonalAppIntro);
    }

    /// Selected row indices for `page`.
    pub fn picked_rows(&self, page: &Page) -> Option<&std::collections::BTreeSet<usize>> {
        self.selection
            .as_ref()
            .filter(|(owner, _, _)| owner == page)
            .map(|(_, _, selection)| &selection.rows)
            .filter(|rows| !rows.is_empty())
    }

    /// Clears selection when the page's rows or order change.
    pub fn keep_picked_rows_for(&mut self, page: &Page, view: &str) {
        let stale = self.selection
            .as_ref()
            .is_some_and(|(owner, seen, _)| owner == page && seen != view);
        if stale {
            self.selection = None;
        }
    }

    /// Applies a single, toggle, or range row selection.
    ///
    /// `len` bounds ranges if rows changed after the anchor was set.
    pub fn pick_row(&mut self, page: &Page, view: &str, row: usize, pick: RowPick, len: usize) {
        let mut selection = match self.selection.take() {
            Some((owner, seen, selection)) if owner == *page && seen == view => selection,
            _ => RowSelection::default(),
        };
        match pick {
            RowPick::Only => {
                // Clicking the sole selected row clears the selection.
                let only_this = selection.rows.len() == 1 && selection.rows.contains(&row);
                selection.rows.clear();
                if only_this {
                    selection.anchor = None;
                } else {
                    selection.rows.insert(row);
                    selection.anchor = Some(row);
                }
            }
            RowPick::Toggle => {
                if !selection.rows.remove(&row) {
                    selection.rows.insert(row);
                }
                selection.anchor = Some(row);
            }
            RowPick::Range => {
                // Without an anchor, shift-click selects only this row.
                let anchor = selection.anchor.unwrap_or(row);
                let (from, to) = if anchor <= row {
                    (anchor, row)
                } else {
                    (row, anchor)
                };
                selection.rows.clear();
                selection.rows.extend((from..=to).filter(|row| *row < len));
                selection.anchor = Some(anchor);
            }
        }
        if selection.rows.is_empty() {
            self.selection = None;
        } else {
            self.selection = Some((page.clone(), view.to_string(), selection));
        }
    }

    /// Picks exactly `rows` of the page's list as it looks in `view`: every
    /// song it shows, for Select all.
    pub fn pick_rows(&mut self, page: &Page, view: &str, rows: std::collections::BTreeSet<usize>) {
        let Some(&first) = rows.first() else {
            self.selection = None;
            return;
        };
        self.selection = Some((
            page.clone(),
            view.to_string(),
            RowSelection {
                rows,
                anchor: Some(first),
            },
        ));
    }

    /// Clears the current row selection.
    pub fn clear_picked_rows(&mut self) {
        self.selection = None;
    }

    /// Whether songs may be added to `playlist` from here: the account
    /// owns it, Spotify flags it collaborative, or the rootlist says the
    /// account was invited to it.
    pub fn can_edit_playlist(&self, playlist: &Playlist) -> bool {
        let owned = self.user_id().is_some_and(|user| playlist.owned_by(user));
        owned || playlist.collaborative || self.editable_by_grant.contains(&playlist.uri)
    }

    /// The playing playlist as a row context, when the now-playing track
    /// comes from a playlist the account may edit. The player bar menu
    /// uses this so removal matches a row's URI-based entry; moves need
    /// a row index and stay in the playlist view.
    pub fn editable_context_playlist(&self) -> Option<RowContext> {
        let uri = self.playing_context_uri()?;
        if !uri.starts_with("spotify:playlist:") {
            return None;
        }
        let id = util::uri_id(&uri)?.to_string();
        let playlist = self
            .library_entry(&id)
            .or_else(|| self.playlist_pages.get(&id)?.playlist.get())?;
        if !self.can_edit_playlist(playlist) {
            return None;
        }
        Some(RowContext::Context {
            uri,
            editable_playlist: Some((id, playlist.snapshot_id.clone())),
        })
    }

    /// The library's playlists that take songs, as id and name pairs.
    pub fn editable_playlists(&self) -> Vec<(String, String)> {
        if self.user_id().is_none() {
            return Vec::new();
        }
        self.library
            .playlists
            .get()
            .map(|playlists| {
                playlists
                    .iter()
                    .filter(|playlist| self.can_edit_playlist(playlist))
                    .map(|playlist| (playlist.id.clone(), playlist.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Engine {
    /// Runs background work with or without a main window.


    /// Closing the window keeps the process running in the tray when the
    /// tray is shown and the setting asks for it.
    ///
    /// This runs with the logic, not the drawing: eframe runs only the logic
    /// of a minimised window, and a close decided while drawing let closing a
    /// minimised window quit the app instead.


    /// Follows the open window's size, position and maximized state, so the
    /// next window of its kind opens as it was left. Not while one window
    /// switches to the other, whose closing frames describe the window going
    /// away, nor while fullscreen lyrics own the main window.


    /// Whether the system reports the connection as metered or charged by
    /// use, so automatic downloads wait.


    /// Whether automatic downloads wait: on a metered connection, or while
    /// the system reports no way to the internet, where a download would
    /// only fail and stop the next automatic one. That report counts only
    /// where the app follows it (macOS): Windows's hint can say there is no
    /// internet where Spotify and GitHub still answer.


    /// Takes an automatic update step now, or holds it until the connection
    /// is neither metered nor down.


    /// Follows the desktop's wish for less motion, which egui's easing and
    /// the interface's own animations then honour.


    /// Follows the system's power and session state: works out the budget
    /// for drawing and polling, tells the backend whether the internet can
    /// be reached, keeps local playback out of App Nap, and answers what the
    /// system is about to do.


    /// Asks for the next pass that playback, pending plays and polling need.
    ///
    /// This runs with the logic, not the drawing: eframe skips drawing a
    /// hidden, minimised or occluded window but still runs its logic, so
    /// a hidden window keeps polling and keeps media controls current.


    /// The soonest that playback, a pending play or polling needs the logic
    /// to run again, if any of them does.


    /// How long the headless loop may sleep after this tick: until the next
    /// pass is due, and never longer than `HEADLESS_SLEEP`, which keeps the
    /// timers in `tick` (saving, eviction, update checks) running. MilkDrop's
    /// window outlives the main one and is polled every tick. Tray clicks,
    /// media keys and backend events wake the loop at once.


    /// Records a track after enough active listening time.
    ///
    /// Paused time and seeking do not count. Each track is recorded once.
    pub fn note_listening(&mut self) {
        let Some(now) = self.now_playing_live() else {
            self.listening = None;
            return;
        };
        let listening = match &mut self.listening {
            Some(held) if held.uri == now.uri => held,
            _ => {
                self.listening = Some(Listening {
                    uri: now.uri.clone(),
                    listened: std::time::Duration::ZERO,
                    playing_since: now.playing.then(Instant::now),
                    recorded: false,
                });
                return;
            }
        };
        match (now.playing, listening.playing_since) {
            // Add the completed interval when playback pauses.
            (false, Some(since)) => {
                listening.listened += since.elapsed();
                listening.playing_since = None;
            }
            (true, None) => listening.playing_since = Some(Instant::now()),
            _ => {}
        }
        if listening.recorded {
            return;
        }
        let listened = listening.listened
            + listening
                .playing_since
                .map(|since| since.elapsed())
                .unwrap_or_default();
        if listened < crate::history::counts_after(now.duration_ms) {
            return;
        }
        listening.recorded = true;
        self.plays
            .record(crate::history::played_track(&now), jiff::Timestamp::now());
        if let Some(bytes) = self.plays.take_serialized() { self.files.write_private(&self.dirs.history_file(), bytes); }
        self.rebuild_recents();
    }

    /// A second after the song playing on another device should be over,
    /// counted from the last poll. At most one early poll goes out for it
    /// every `REMOTE_POLL_MIN`.
    pub fn remote_song_end(&self) -> Option<Duration> {
        if matches!(self.target(), Target::Local)
            || self.remote_end_polled_at
                .is_some_and(|at| at.elapsed() < REMOTE_POLL_MIN)
        {
            return None;
        }
        let remote = self
            .remote_fresh()
            .filter(|remote| remote.state.is_playing)?;
        let left = remote
            .state
            .item
            .as_ref()?
            .duration_ms()
            .checked_sub(remote.state.progress_ms?)
            .filter(|left| *left > 0)?;
        let due = remote.received_at + Duration::from_millis(left.into()) + Duration::from_secs(1);
        due.checked_duration_since(self.remote_polled_at)
    }

}
impl Engine {
    pub fn ensure_liked_songs(&mut self) {
        if self.liked_songs.cache_loading || self.liked_songs.refreshing() {
            return;
        }
        if !self.liked_songs.cache_checked {
            if self.user_id().is_none() {
                return;
            }
            self.load_generation = self.load_generation.wrapping_add(1);
            self.liked_songs.generation = self.load_generation;
            self.liked_songs.cache_loading = true;
            self.library.liked.loading = true;
            self.backend.send(Command::LoadLikedSongsCache {
                generation: self.load_generation,
            });
        } else if !self.liked_songs.fresh(jiff::Timestamp::now().as_second())
            && self.library.liked.error.is_none()
            && self.network_reachable
        {
            self.refresh_liked_songs();
        }
    }

    pub fn receive_liked_cache(
        &mut self,
        account: &str,
        generation: u64,
        cache: Option<crate::liked::Cache>,
    ) {
        if self.user_id() != Some(account)
            || self.liked_songs.generation != generation
            || self.liked_songs.cache_checked
        {
            return;
        }
        self.liked_songs.cache_loading = false;
        self.liked_songs.cache_checked = true;
        if let Some(cache) = cache.filter(|cache| cache.valid_for(account)) {
            self.liked_songs.restore(cache);
        }
        self.sync_liked_songs();
        for item in &self.library.liked.items {
            let uri = &item.track.uri;
            if !self.saved_writes.contains_key(uri) && self.liked_songs.intent(uri).is_none() {
                self.saved.insert(uri.clone(), true);
                if let Some(key) = item.track.recording_key() {
                    link_recording(
                        &mut self.track_recordings,
                        &mut self.recording_uris,
                        uri.clone(),
                        key.clone(),
                    );
                    self.saved_recordings.insert(key);
                }
            }
        }
        if (!self.liked_songs.fresh(jiff::Timestamp::now().as_second())
            || self.liked_songs.has_confirmed_changes())
            && self.network_reachable
        {
            self.refresh_liked_songs();
        } else if self.table_sorts.contains_key(&Page::LikedSongs) && self.network_reachable {
            self.load_more(Page::LikedSongs);
        }
    }

    pub fn refresh_liked_songs(&mut self) {
        if !self.liked_songs.cache_checked {
            self.ensure_liked_songs();
            return;
        }
        self.load_generation = self.load_generation.wrapping_add(1);
        self.liked_songs.start_refresh(self.load_generation);
        self.library.liked.loading = true;
        self.load_more(Page::LikedSongs);
    }

    /// Whether Liked Songs at `revision` is a prefix of the rows shown now.
    pub fn liked_view_extends(&self, revision: u64) -> bool {
        self.liked_songs.view_extends(&self.library.liked, revision)
    }

    pub fn sync_liked_songs(&mut self) {
        self.liked_songs.sync_view(&mut self.library.liked);
        for (uri, saved) in self.liked_songs.intents() {
            self.set_saved_state(uri, saved);
        }
    }

    pub fn checkpoint_liked_songs(&mut self, force: bool) {
        if let Some(account) = self.user_id().map(str::to_owned)
            && let Some(cache) = self.liked_songs.checkpoint(account, force)
        {
            self.backend.send(Command::StoreLikedSongsCache(cache));
        }
    }

    pub fn change_liked_song(&mut self, uri: &str, saved: bool) -> bool {
        let Some(id) = uri.strip_prefix("spotify:track:") else {
            return false;
        };
        self.liked_songs.seed(&self.library.liked);
        let track = self.track_cache
            .get(id)
            .cloned()
            .or_else(|| {
                self.library
                    .liked
                    .items
                    .iter()
                    .find(|item| item.track.uri == uri)
                    .map(|item| item.track.clone())
            })
            .or_else(|| {
                self.now_playing_item().and_then(|item| match item {
                    PlayableItem::Track(track) if track.uri == uri => Some(track),
                    _ => None,
                })
            })
            .or_else(|| {
                self.table_rows.values().find_map(|table| {
                    table.items.iter().find_map(|(item, _, _)| match item {
                        PlayableItem::Track(track) if track.uri == uri => Some(track.clone()),
                        _ => None,
                    })
                })
            })
            .or_else(|| {
                self.home
                    .top_tracks
                    .get()
                    .and_then(|tracks| tracks.iter().find(|track| track.uri == uri).cloned())
            })
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.tracks.as_ref())
                    .and_then(|tracks| tracks.items.iter().find(|track| track.uri == uri).cloned())
            });
        let missing = track.is_none();
        let track = track.unwrap_or_else(|| Track {
            uri: uri.to_string(),
            id: Some(id.to_string()),
            name: self.localize.t(self.locale, "Loading…").into_owned(),
            ..Default::default()
        });
        self.liked_songs.change(uri.to_string(), saved, track);
        if saved && missing && self.track_requests.insert(id.to_string()) {
            self.backend.api(ApiRequest::Track { id: id.to_string() });
        }
        true
    }
}

pub fn play_pause_label(playing: bool) -> &'static str {
    if playing { "Pause" } else { "Play" }
}

pub fn link_recording(
    track_recordings: &mut HashMap<String, String>,
    recording_uris: &mut HashMap<String, HashSet<String>>,
    uri: String,
    key: String,
) {
    if let Some(previous) = track_recordings.insert(uri.clone(), key.clone())
        && previous != key
        && let Some(uris) = recording_uris.get_mut(&previous)
    {
        uris.remove(&uri);
        if uris.is_empty() {
            recording_uris.remove(&previous);
        }
    }
    recording_uris.entry(key).or_default().insert(uri);
}

pub fn same_recording_hint(left: &Track, right: &Track) -> bool {
    left.uri != right.uri
        && !left.name.is_empty()
        && left.name == right.name
        && left.duration_ms > 0
        && left.duration_ms == right.duration_ms
        && !left.artists.is_empty()
        && left.artists.len() == right.artists.len()
        && left
            .artists
            .iter()
            .zip(&right.artists)
            .all(|(left, right)| match (&left.id, &right.id) {
                (Some(left), Some(right)) => left == right,
                _ => left.name == right.name,
            })
}

pub fn increase_playlist_total(playlist: &mut Playlist, added: u32) {
    if let Some(items) = &mut playlist.items_count {
        items.total = items.total.saturating_add(added);
    } else if let Some(tracks) = &mut playlist.tracks {
        tracks.total = tracks.total.saturating_add(added);
    } else {
        playlist.items_count = Some(TrackCount { total: added });
    }
}

pub fn page_related_needs_load(pages: &HashMap<String, ArtistPage>, id: &str) -> bool {
    pages.get(id).is_some_and(|page| page.related.needs_load())
}

pub fn known_availability(items: &[PlaylistItem]) -> impl Iterator<Item = (&str, bool)> {
    items.iter().filter_map(|item| match item.playable()? {
        PlayableItem::Track(track) => Some((track.uri.as_str(), track.is_playable?)),
        _ => None,
    })
}

pub fn note_availability(availability: &mut HashMap<String, bool>, items: &[PlaylistItem]) {
    availability
        .extend(known_availability(items).map(|(uri, playable)| (uri.to_string(), playable)));
}

pub fn fill_availability(availability: &HashMap<String, bool>, items: &mut [PlaylistItem]) -> bool {
    let mut changed = false;
    for item in items {
        if let Some(PlayableItem::Track(track)) = item.item.as_mut()
            && track.is_playable.is_none()
            && availability.get(&track.uri) == Some(&false)
        {
            track.is_playable = Some(false);
            changed = true;
        }
    }
    changed
}

pub fn local_load(request: &PlayRequest, shuffle: bool) -> LoadSpec {
    let single_song = request.context_uri.is_none()
        && request.uris.len() == 1
        && request.uris[0].contains(":track:");
    if single_song {
        return LoadSpec {
            context_uri: Some(request.uris[0].clone()),
            position_ms: request.position_ms,
            play: true,
            ..LoadSpec::default()
        };
    }
    // A plain track list must not load shuffled when a row was chosen:
    // librespot shuffles the list first and then cannot find the chosen
    // row in it, falls back to nowhere, and replays what was on. The list
    // loads straight and shuffle is switched on right after the load,
    // which also matches Spotify: the chosen song plays, the rest shuffle.
    let chosen = request.offset_uri.is_some() || request.offset_position.is_some();
    let list = request.context_uri.is_none();
    LoadSpec {
        context_uri: request.context_uri.clone(),
        uris: request.uris.clone(),
        offset_uri: request.offset_uri.clone(),
        offset_index: request.offset_position,
        position_ms: request.position_ms,
        play: true,
        shuffle: (shuffle && !(list && chosen)).then_some(true),
        repeat: None,
        autoplay: false,
    }
}

pub fn autoplay_seed(
    list: Option<&[String]>,
    autoplay: bool,
    before: &LocalState,
    after: &LocalState,
) -> Option<String> {
    if !autoplay
        || !after.connected
        || after.playback != Playback::Stopped
        || before.playback != Playback::Playing
    {
        return None;
    }
    let track = before.track.as_ref()?;
    if list?.last() != Some(&track.uri) || track.duration_ms == 0 {
        return None;
    }
    let near_end = before.position_now() + 3_000 >= track.duration_ms;
    near_end.then(|| track.uri.clone())
}

pub fn cap_uris(uris: &[String], index: u32) -> (Vec<String>, u32) {
    const MAX: usize = 500;
    if uris.len() <= MAX {
        return (uris.to_vec(), index);
    }
    let start = (index as usize).min(uris.len().saturating_sub(1));
    let end = (start + MAX).min(uris.len());
    (uris[start..end].to_vec(), 0)
}

pub fn evict_lru_map<V>(
    map: &mut HashMap<String, V>,
    used: &HashMap<Page, Instant>,
    to_page: impl Fn(&str) -> Page,
    protected: &HashSet<String>,
    max: usize,
) {
    if map.len() <= max {
        return;
    }
    let overflow = map.len() - max;
    let mut victims: Vec<(Option<Instant>, String)> = map
        .keys()
        .filter(|id| !protected.contains(*id))
        .map(|id| {
            let used = used.get(&to_page(id)).copied();
            (used, id.clone())
        })
        .collect();
    victims.sort();
    for (_, id) in victims.into_iter().take(overflow) {
        map.remove(&id);
    }
}

pub fn cover_images(cover: &crate::playlist_cover::Cover) -> Vec<crate::api::models::Image> {
    vec![crate::api::models::Image {
        url: cover.uri.clone(),
        width: None,
        height: None,
    }]
}

pub fn remote_action_label(localize: &dyn Localize, locale: Locale, action: RemoteAction) -> Cow<'static, str> {
    match action {
        RemoteAction::Play => localize.t(locale, "Couldn't start playback"),
        RemoteAction::Pause => localize.t(locale, "Couldn't pause"),
        RemoteAction::Next => localize.t(locale, "Couldn't skip"),
        RemoteAction::Previous => localize.t(locale, "Couldn't go back"),
        RemoteAction::Seek => localize.t(locale, "Couldn't seek"),
        RemoteAction::Volume => localize.t(locale, "Couldn't change the volume"),
        RemoteAction::Shuffle => localize.t(locale, "Couldn't change shuffle"),
        RemoteAction::Repeat => localize.t(locale, "Couldn't change repeat"),
    }
}

pub fn engine_error_text(localize: &dyn Localize, locale: Locale, error: &str) -> String {
    if error == crate::sink::NO_DEVICE {
        return localize.t(
        locale,
            "No audio output device was found. Connect or enable one, then press play again.",
        )
        .into_owned();
    }
    error.to_owned()
}

pub fn friendly_page_error(localize: &dyn Localize, locale: Locale, error: &crate::api::ApiError) -> String {
    if error.is_network() {
        return localize.t(locale, "Network connection error. Check your connection.").into_owned();
    }
    match error.status() {
        Some(403) | Some(404) => localize.t(
        locale,
            "Spotify doesn't make this playlist's songs available to third-party apps.",
        )
        .into_owned(),
        _ => error.to_string(),
    }
}

pub fn friendly_api_error(localize: &dyn Localize, locale: Locale, error: &crate::api::ApiError) -> String {
    if error.is_network() {
        localize.t(locale, "Network connection error. Check your connection.").into_owned()
    } else {
        error.to_string()
    }
}

pub fn cover_error(localize: &dyn Localize, locale: Locale, error: &crate::api::client::ApiError) -> String {
    match error.status() {
        Some(401) => localize.t(
        locale,
            "Spotify sign-in expired. Sign in again, then retry the cover upload.",
        )
        .into_owned(),
        Some(403) => localize.t(
        locale,
            "Spotify refused this cover. Check that you own the playlist, then sign in again to grant image upload permission. If using a personal app, reconnect it in Settings too.",
        )
        .into_owned(),
        Some(413) => {
            localize.t(locale, "Spotify rejected the image size. Choose a smaller image.").into_owned()
        }
        // Translators: {error} is an error message.
        _ => localize.t(locale, "Couldn't upload the cover: {error}. Try again.")
            .replace("{error}", &error.to_string()),
    }
}

impl Engine {
    pub fn reset_data(&mut self) {
        self.local_transfer_sequence = None;
        self.queue_start_pending = None;
        self.pending_album_queues.clear();
        self.pending_queue_batches.clear();
        self.last_album_queue = None;
        self.cover_uploads.clear();
        self.uploaded_covers.clear();
        // Pages still on their way belong to the signed-out account's load.
        self.library = Library {
            playlists_generation: self.library.playlists_generation,
            ..Library::default()
        };
        self.liked_songs = crate::liked::LikedSongs::default();
        self.liked_recheck_at = None;
        // Answers still on their way belong to the signed-out account's
        // Home.
        self.home = HomeData {
            generation: self.home.generation,
            top_songs_generation: self.home.top_songs_generation,
            made_for_you_generation: self.home.made_for_you_generation,
            ..HomeData::default()
        };
        self.playlist_pages.clear();
        self.album_pages.clear();
        self.prerelease_request = None;
        self.artist_pages.clear();
        self.show_pages.clear();
        self.album_types_requested.clear();
        self.audiobook_shows.clear();
        self.audiobooks_requested.clear();
        self.playlist_headers.clear();
        self.playlist_headers_requested.clear();
        self.confirmed_ep_albums.clear();
        self.saved.clear();
        self.saved_pending.clear();
        self.track_recordings.clear();
        self.recording_uris.clear();
        self.playlist_availability.clear();
        self.saved_recordings.clear();
        self.saved_writes.clear();
        self.queue = Loadable::NotLoaded;
        self.queue_shuffle_pending = None;
        self.queue_reorder_pending = None;
        self.queue_removed = None;
        self.devices.clear();
        self.control_devices_stale = true;
        self.devices_fetched_at = None;
        self.search.results = Loadable::NotLoaded;
        self.search.committed.clear();
        self.search.committed_run = None;
        self.search.typed_entry = None;
        self.search.playlists = None;
        self.search.serial += 1;
        self.search.catalogue_pending = false;
        self.search.playlists_pending = false;
        self.search.error = None;
        self.table_rows.clear();
        self.page_used.clear();
        self.track_used.clear();
        self.copied_songs.clear();
        self.pending_pastes.clear();
    }

    pub fn station_name(&self, context: &str) -> Option<String> {
        self.radio_name(&util::station_seed(context)?)
    }



    pub fn ensure_loaded(&mut self, page: Page) {
        if !self.is_connected() {
            return;
        }
        match page {
            Page::Home => {
                if self.network_reachable {
                    // A refresh that failed behind cached shelves is read
                    // again on opening Home, without waiting ten minutes.
                    self.load_home(self.home.refresh_failed);
                }
            }
            Page::TopSongs => {
                if self.network_reachable {
                    self.load_top_songs(false);
                }
            }
            Page::Search => {}
            Page::LikedSongs => self.ensure_liked_songs(),
            Page::Albums => {
                if self.network_reachable && !self.library.albums.loaded_once {
                    self.load_more(Page::Albums);
                }
            }
            Page::Artists => {
                if self.network_reachable && !self.library.artists.loaded_once {
                    self.load_more(Page::Artists);
                }
            }
            Page::Podcasts => {
                if self.network_reachable && !self.library.shows.loaded_once {
                    self.load_more(Page::Podcasts);
                }
            }
            Page::Episodes => {
                if self.network_reachable && !self.library.episodes.loaded_once {
                    self.load_more(Page::Episodes);
                }
            }
            Page::Playlist(id) => {
                let needs_generation = self.playlist_pages
                    .get(&id)
                    .is_none_or(|page| page.generation == 0);
                if needs_generation {
                    self.load_generation += 1;
                    self.playlist_pages
                        .entry(id.clone())
                        .or_default()
                        .generation = self.load_generation;
                }
                let page = self.playlist_pages.entry(id.clone()).or_default();
                let generation = page.generation;
                if page.playlist.needs_load() && self.network_reachable {
                    page.playlist = Loadable::Loading;
                    self.backend.api(ApiRequest::Playlist {
                        id: id.clone(),
                        generation,
                    });
                }
                if !page.items.loaded_once && page.items.can_load_more() {
                    if self.network_reachable {
                        page.items.loading = true;
                        self.backend.api(ApiRequest::PlaylistItems {
                            id: id.clone(),
                            offset: 0,
                            generation,
                        });
                    }
                    // Disk progress is adopted only if Spotify's snapshot
                    // still matches.
                    self.backend.send(Command::LoadPlaylistCache {
                        id: id.clone(),
                        generation,
                    });
                }
                if self.network_reachable {
                    self.request_contains(vec![format!("spotify:playlist:{id}")]);
                }
            }
            Page::Album(id) => {
                if !self.album_pages.contains_key(&id) {
                    self.load_generation = self.load_generation.wrapping_add(1);
                    self.album_pages.insert(
                        id.clone(),
                        AlbumPage {
                            generation: self.load_generation,
                            ..Default::default()
                        },
                    );
                }
                let page = self.album_pages.entry(id.clone()).or_default();
                if page.album.needs_load() && self.network_reachable {
                    page.album = Loadable::Loading;
                    self.backend.api(ApiRequest::Album { id: id.clone() });
                }
                if self.network_reachable {
                    self.request_contains(vec![format!("spotify:album:{id}")]);
                }
            }
            Page::Artist(id) => {
                let page = self.artist_pages.entry(id.clone()).or_default();
                if self.network_reachable {
                    if page.artist.needs_load() {
                        page.artist = Loadable::Loading;
                        self.backend.api(ApiRequest::Artist { id: id.clone() });
                    }
                    let filter = page.filter;
                    self.load_artist_albums(&id, filter);
                    if page_related_needs_load(&self.artist_pages, &id) {
                        if let Some(page) = self.artist_pages.get_mut(&id) {
                            page.related = Loadable::Loading;
                        }
                        self.backend
                            .api(ApiRequest::RelatedArtists { id: id.clone() });
                    }
                    self.request_contains(vec![format!("spotify:artist:{id}")]);
                }
            }
            Page::Show(id) => {
                let page = self.show_pages.entry(id.clone()).or_default();
                if page.show.needs_load() && self.network_reachable {
                    page.show = Loadable::Loading;
                    self.backend.api(ApiRequest::Show { id: id.clone() });
                }
                if self.network_reachable {
                    self.request_contains(vec![format!("spotify:show:{id}")]);
                }
            }
            Page::Radio(seed) => {
                if self.network_reachable {
                    self.load_radio(&seed);
                }
            }
            Page::Queue => {
                if self.network_reachable {
                    self.refresh_queue(true);
                }
            }
            Page::Settings => {}
        }
    }

    pub fn reload(&mut self, page: Page) {
        match &page {
            Page::Home => self.load_home(true),
            Page::TopSongs => self.load_top_songs(true),
            Page::LikedSongs => {
                self.refresh_liked_songs();
                return;
            }
            Page::Albums => self.library.albums.reset(),
            Page::Artists => self.library.artists.reset(),
            Page::Podcasts => self.library.shows.reset(),
            Page::Episodes => self.library.episodes.reset(),
            Page::Playlist(id) => {
                if let Some(playlist) = self.playlist_pages.get_mut(id) {
                    playlist.items.loading = true;
                    playlist.items.error = None;
                    if playlist.pending_writes > 0 || playlist.optimistic_snapshot.is_some() {
                        // A refresh must not turn pre-write rows into a new,
                        // apparently current generation. Wait for the write's
                        // metadata confirmation before asking for rows.
                        playlist.refresh_after_write = true;
                        playlist.snapshot_rechecks = 0;
                        if playlist.pending_writes == 0 {
                            self.backend.api(ApiRequest::Playlist {
                                id: id.clone(),
                                generation: playlist.generation,
                            });
                        }
                        return;
                    }
                    playlist.refresh_after_write = false;
                    self.load_generation += 1;
                    playlist.generation = self.load_generation;
                    playlist.items.clear_windows();
                    playlist.cache_checked = true;
                    playlist.cache_restored_through = None;
                    playlist.pending_cache = None;
                    playlist.cache_append_valid = false;
                    playlist.local_additions.clear();
                    playlist.optimistic_snapshot = None;
                    playlist.snapshot_rechecks = 0;
                    self.backend.api(ApiRequest::Playlist {
                        id: id.clone(),
                        generation: playlist.generation,
                    });
                    self.backend.api(ApiRequest::PlaylistItems {
                        id: id.clone(),
                        offset: 0,
                        generation: playlist.generation,
                    });
                    return;
                }
            }
            Page::Album(id) => {
                self.album_pages.remove(id);
            }
            Page::Artist(id) => {
                self.artist_pages.remove(id);
            }
            Page::Show(id) => {
                self.show_pages.remove(id);
            }
            Page::Radio(seed) => {
                // A new mix replaces the songs on screen when it arrives.
                if self.refresh_radio(seed) {
                    return;
                }
                self.radio_pages.remove(seed);
            }
            Page::Queue => self.queue = Loadable::NotLoaded,
            _ => {}
        }
        self.ensure_loaded(page);
    }





    pub fn flush_files(&self) -> bool {
        self.files.flush(SUSPEND_FLUSH_TIMEOUT)
    }

}

// Radio pages: Spotify's station for a song, playlist, album, or artist.
//
// Spotify mixes a station afresh each time it is resolved, so the page
// keeps the songs it was given and its Play button plays exactly those,
// with the station as their context so the queue names the radio.


impl Engine {
    /// "`<Name>` Radio" for a seed whose name is known.
    pub fn radio_name(&self, seed: &str) -> Option<String> {
        let id = util::uri_id(seed)?;
        let name = match util::uri_kind(seed)? {
            "track" => self.track_cache.get(id).map(|track| track.name.clone()),
            "playlist" => self.playlist_pages
                .get(id)
                .and_then(|page| page.playlist.get())
                .or_else(|| self.known_playlist(id))
                .map(|playlist| playlist.name.clone()),
            "album" => self.album_pages
                .get(id)
                .and_then(|page| page.album.get())
                .or_else(|| self.known_album(id))
                .map(|album| album.name.clone()),
            "artist" => self.artist_pages
                .get(id)
                .and_then(|page| page.artist.get())
                .or_else(|| self.known_artist(id))
                .map(|artist| artist.name.clone()),
            _ => None,
        }?;
        Some(
            // Translators: Keep {track} unchanged. It is the name of the song,
            // playlist, album, or artist the radio is based on, not translated.
            self.localize.t(self.locale, "{track} Radio").replace("{track}", &name),
        )
    }

    /// The seed's artwork, for the radio page's cover.
    pub fn radio_images(&self, seed: &str) -> Vec<crate::api::models::Image> {
        let Some(id) = util::uri_id(seed) else {
            return Vec::new();
        };
        let images = match util::uri_kind(seed) {
            Some("track") => self.track_cache
                .get(id)
                .and_then(|track| track.album.as_ref())
                .map(|album| album.images.clone()),
            Some("playlist") => self.playlist_pages
                .get(id)
                .and_then(|page| page.playlist.get())
                .or_else(|| self.known_playlist(id))
                .map(|playlist| playlist.images.clone()),
            Some("album") => self.album_pages
                .get(id)
                .and_then(|page| page.album.get())
                .or_else(|| self.known_album(id))
                .map(|album| album.images.clone()),
            Some("artist") => self.artist_pages
                .get(id)
                .and_then(|page| page.artist.get())
                .or_else(|| self.known_artist(id))
                .map(|artist| artist.images.clone()),
            _ => None,
        };
        images.unwrap_or_default()
    }

    /// Opens the radio of a song shown in a list. The page takes its name
    /// and cover from the song cache, which holds only songs that played or
    /// came back from a radio, so a song from Liked Songs, a playlist or a
    /// search opened as an unnamed "Radio" with no cover (#644). The row
    /// already has the song's details, so cache them first. A cached entry
    /// is kept, but takes the row's album when it has no artwork of its own:
    /// an album's song list comes without the album.
    pub fn open_song_radio(&mut self, uri: &str, track: &Track) {
        if let Some(id) = util::uri_id(uri) {
            let cached = self.track_cache
                .entry(id.to_owned())
                .or_insert_with(|| track.clone());
            let has_art = |t: &Track| t.album.as_ref().is_some_and(|a| !a.images.is_empty());
            if !has_art(cached) && has_art(track) {
                cached.album = track.album.clone();
            }
            self.track_used.insert(id.to_owned(), Instant::now());
        }
        self.open(Page::Radio(uri.to_string()));
    }

    /// Asks for the seed's songs unless the page already has or awaits them.
    pub(super) fn load_radio(&mut self, seed: &str) {
        let name = self.radio_name(seed);
        let images = self.radio_images(seed);
        let page = self.radio_pages.entry(seed.to_string()).or_default();
        if name.is_some() {
            page.name = name;
        }
        if !images.is_empty() {
            page.images = images;
        }
        if !page.songs.needs_load() {
            return;
        }
        self.load_generation = self.load_generation.wrapping_add(1);
        page.generation = self.load_generation;
        page.songs = Loadable::Loading;
        self.backend.send(Command::Radio {
            seed: seed.to_string(),
            generation: page.generation,
        });
    }

    /// Asks for a new mix while the page keeps showing its songs. False
    /// when there are no songs to keep, so the page loads afresh.
    pub(super) fn refresh_radio(&mut self, seed: &str) -> bool {
        let Some(page) = self.radio_pages
            .get_mut(seed)
            .filter(|page| page.songs.get().is_some())
        else {
            return false;
        };
        if !page.refreshing {
            self.load_generation = self.load_generation.wrapping_add(1);
            page.generation = self.load_generation;
            page.refreshing = true;
            self.backend.send(Command::Radio {
                seed: seed.to_string(),
                generation: page.generation,
            });
        }
        true
    }

    pub fn receive_radio(
        &mut self,
        seed: &str,
        generation: u64,
        result: Result<Vec<Track>, String>,
    ) {
        let Some(page) = self.radio_pages
            .get_mut(seed)
            .filter(|page| page.generation == generation)
        else {
            return;
        };
        let refreshing = std::mem::take(&mut page.refreshing);
        match result {
            Ok(songs) => {
                let uris = songs.iter().map(|track| track.uri.clone()).collect();
                for track in &songs {
                    if let Some(id) = &track.id {
                        self.track_cache
                            .entry(id.clone())
                            .or_insert_with(|| track.clone());
                    }
                }
                page.songs = Loadable::Loaded(songs);
                // The table's cached rows follow the new songs.
                self.load_generation = self.load_generation.wrapping_add(1);
                page.generation = self.load_generation;
                self.request_contains(uris);
            }
            // A failed refresh keeps the mix on screen and says why.
            Err(error) if refreshing => self.toast(error),
            Err(error) => page.songs = Loadable::Failed(error),
        }
    }

    /// Saves the radio's songs, as shown, to a new playlist named after it.
    pub fn save_radio(&mut self, seed: &str) {
        let Some(songs) = self.radio_pages.get(seed).and_then(|page| page.songs.get()) else {
            return;
        };
        let add_uris: Vec<String> = songs.iter().map(|track| track.uri.clone()).collect();
        if add_uris.is_empty() {
            return;
        }
        let name = self
            .radio_name(seed)
            .or_else(|| self.radio_pages.get(seed)?.name.clone())
            .unwrap_or_else(|| self.localize.t(self.locale, "Radio").into_owned());
        self.actions.push(Action::CreatePlaylist {
            name,
            public: false,
            add_uris,
        });
    }
}

impl Engine {
    pub fn tint_for(&mut self, url: Option<&str>) -> Option<[u8; 3]> {
        let url = url?;
        if let Some(color) = self.accents.get(url) {
            return Some(*color);
        }
        if self.accent_pending.insert(url.to_string()) {
            self.backend.send(Command::Accent { url: url.to_string() });
        }
        None
    }
}

impl Engine {
    pub fn handle_local(&mut self, state: LocalState) {
        if self.local_transfer_sequence
            .is_some_and(|sequence| sequence != state.track_sequence)
        {
            self.local_transfer_sequence = None;
        }
        if state.track_sequence != self.local.track_sequence
            && matches!(self.target(), Target::Local)
        {
            self.listening = None;
        }
        let track_changed =
            state.track != self.local.track || state.track_sequence != self.local.track_sequence;
        let reconnected = state.connected && !self.local.connected;
        if state.shuffle != self.local.shuffle
            && self.shuffle_set_at
                .is_none_or(|at| at.elapsed() > Duration::from_secs(5))
        {
            // Accept shuffle changes made by another client.
            self.shuffle_wanted = state.shuffle;
        }
        if state.playback != self.local.playback {
            self.optimistic_playing = None;
            if matches!(state.playback, Playback::Playing | Playback::Loading) {
                self.clear_play_pending();
            }
        }
        if state.track != self.local.track {
            self.clear_play_pending();
        }
        self.reconcile_local_track_intent(
            track_changed,
            state.track.as_ref().map(|track| track.uri.as_str()),
        );
        let held_volume = self.held_local_volume(state.volume);
        if held_volume.is_none() && state.volume != self.settings.volume {
            self.settings.volume = state.volume;
            self.settings_dirty = true;
        }
        if state.seek_sequence != self.local.seek_sequence || track_changed {
            self.pending_local_position = None;
        }
        if let Some(error) = &state.error
            && self.local.error.as_deref() != Some(error.as_str())
        {
            self.toast_error(engine_error_text(self.localize.as_ref(), self.locale, error));
            // One unavailable track is Spotify's catalogue; several in a
            // row is the session's audio-key service gone bad, which
            // leaves librespot feeding the decoder encrypted bytes and
            // skipping through the whole album. A fresh session cures it.
            if error.starts_with("This item isn't available") {
                let now = Instant::now();
                self.unavailable_at
                    .retain(|at| now.duration_since(*at) < Duration::from_secs(20));
                self.unavailable_at.push(now);
                if self.unavailable_at.len() >= 3
                    && self.last_unavailable_reconnect
                        .is_none_or(|at| at.elapsed() > Duration::from_secs(60))
                {
                    self.unavailable_at.clear();
                    self.last_unavailable_reconnect = Some(now);
                    self.backend.send(Command::Reconnect);
                    self.toast(self.localize.t(
                        self.locale,
                        "Spotify audio disconnected. Reconnecting local playback",
                    ));
                }
            }
        }
        if let Some(seed) = autoplay_seed(
            self.local_list.as_deref(),
            self.settings.autoplay,
            &self.local,
            &state,
        ) {
            log::info!("the list ended; playing what Spotify follows {seed} with");
            self.local_list = None;
            self.backend.player(PlayerCommand::Load(LoadSpec {
                context_uri: Some(seed),
                play: true,
                autoplay: true,
                ..LoadSpec::default()
            }));
        }
        // Another device may have taken over while the last poll still
        // names this one.
        if self.local.is_active() && !state.is_active() {
            self.poll_remote_soon();
        }
        self.local = state;
        if let Some(volume) = held_volume {
            self.local.volume = volume;
        }
        if track_changed {
            self.on_now_playing_changed();
        }
        if reconnected {
            self.replay_queued_play();
            // Retry names requested before the session connected.
            let unresolved: Vec<String> = self.user_names
                .iter()
                .filter(|(_, name)| name.is_none())
                .map(|(id, _)| id.clone())
                .collect();
            for id in &unresolved {
                self.user_names.remove(id);
            }
            self.request_user_names(unresolved);
        }
    }

    pub fn handle_api(&mut self, mut response: ApiResponse) {
        match &mut response {
            ApiResponse::Playlist {
                id,
                result: Ok(playlist),
                generation,
            } => {
                if self.playlist_pages
                    .get(id)
                    .is_some_and(|page| page.generation == *generation)
                {
                    self.reconcile_playlist_cover(id, &mut playlist.images);
                }
            }
            ApiResponse::MyPlaylists {
                result: Ok(page), ..
            } => {
                for playlist in &mut page.items {
                    self.reconcile_playlist_cover(&playlist.id, &mut playlist.images);
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
                    if free && !self.premium_notice_shown {
                        self.premium_notice_shown = true;
                        self.dialog = Some(Dialog::PremiumNeeded);
                    }
                    let due = std::mem::take(&mut self.account_caches_due);
                    let changed = self.user_id() != Some(user.id.as_str());
                    if changed {
                        self.rootlist = self.rootlist_cache
                            .as_ref()
                            .filter(|cached| cached.account_id == user.id)
                            .map(|cached| cached.entries.clone())
                            .unwrap_or_default();
                        self.editable_by_grant.clear();
                    }
                    if changed || due {
                        self.request_account_caches();
                    }
                    self.user = Some(user);
                    let page = self.page().clone();
                    self.ensure_loaded(page);
                    if let Some(now) = self.now_playing() {
                        self.request_contains(vec![now.uri]);
                    }
                }
                Err(error) => {
                    if matches!(error, crate::api::ApiError::SignInExpired { .. }) {
                        self.auth = AuthStatus::Failed(
                            self.localize.t(
                                self.locale,
                                "Your Spotify sign-in expired. Please sign in again.",
                            )
                            .into_owned(),
                        );
                    } else {
                        self.toast_error(
                            // Translators: {error} is an error message.
                            self.localize.t(self.locale, "Couldn't load your profile: {error}")
                                .replace("{error}", &error.to_string()),
                        );
                    }
                }
            },
            ApiResponse::Devices(result) => {
                self.devices_loading = false;
                self.devices_fetched_at = Some(Instant::now());
                match result {
                    Ok(devices) => {
                        self.devices = devices;
                        self.control_devices_stale = true;
                        if let Some((name, since)) = self.pending_transfer_to.clone() {
                            // The receiver's own id keeps another device of the
                            // same name from taking over. A rescan can drop the
                            // receiver; then the name has to do.
                            let ids: Vec<_> = self.receivers
                                .iter()
                                .filter(|receiver| receiver.name == name)
                                .filter_map(|receiver| receiver.device_id.clone())
                                .collect();
                            let matching = self.devices
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
                                self.pending_transfer_to = None;
                                let play = self.activating_play
                                    || self.now_playing().is_some_and(|now| now.playing);
                                self.transfer_remote(id, play);
                            } else if since.elapsed() > Duration::from_secs(20) {
                                self.pending_transfer_to = None;
                            } else {
                                self.devices_fetched_at = None;
                            }
                        }
                        if let Some(selected) = &self.selected_device
                            && !self.receivers.iter().any(|receiver| {
                                crate::sonos::is_sonos(receiver)
                                    && receiver.device_id.as_deref() == Some(selected.as_str())
                            })
                            && !self.devices
                                .iter()
                                .any(|device| device.id.as_deref() == Some(selected.as_str()))
                        {
                            self.selected_device = None;
                        }
                    }
                    Err(error) => self.toast_error(
                        // Translators: {error} is an error message.
                        self.localize.t(self.locale, "Couldn't list devices: {error}")
                            .replace("{error}", &error.to_string()),
                    ),
                }
            }
            ApiResponse::PlaybackState { seq, result } => {
                if seq != self.remote_poll_seq {
                    // An older poll finishing late describes the past.
                    return;
                }
                self.remote_poll_pending = false;
                match result {
                    Ok(state) => {
                        let previous_uri = self.remote.as_ref().and_then(|remote| {
                            remote
                                .state
                                .item
                                .as_ref()
                                .map(|item| item.uri().to_string())
                        });
                        let previous_shuffle = self.remote
                            .as_ref()
                            .map(|remote| remote.state.shuffle_state);
                        self.remote = state.map(|state| RemoteSnapshot {
                            state,
                            received_at: Instant::now(),
                        });
                        if let (Some(previous), Some(current)) = (
                            previous_shuffle,
                            self.remote
                                .as_ref()
                                .map(|remote| remote.state.shuffle_state),
                        ) && previous != current
                            && self.shuffle_set_at
                                .is_none_or(|at| at.elapsed() > Duration::from_secs(5))
                        {
                            // Accept shuffle changes from another device.
                            self.shuffle_wanted = current;
                        }
                        if let Some(context) = self.remote
                            .as_ref()
                            .and_then(|remote| remote.state.context.as_ref())
                            .map(|context| context.uri.clone())
                        {
                            // Ignore the old context while a takeover settles.
                            let stale = self.assumed_context.as_ref().is_some_and(|assumed| {
                                assumed.at.elapsed() < ASSUMED_CONTEXT_HOLD
                                    && assumed.uri != context
                            });
                            if !stale {
                                self.note_recent_context(&context);
                            }
                        }
                        let uri = self.remote.as_ref().and_then(|remote| {
                            remote
                                .state
                                .item
                                .as_ref()
                                .map(|item| item.uri().to_string())
                        });
                        self.reconcile_remote_track_intent(seq, uri.as_deref());
                        if let Some(remote) = &self.remote
                            && let Some(device) = &remote.state.device
                            && device.id.is_some()
                            && let Some(known) =
                                self.devices.iter_mut().find(|known| known.id == device.id)
                        {
                            known.is_active = true;
                            known.volume_percent = device.volume_percent;
                        }
                        if let Some((wanted, _)) = self.optimistic_playing
                            && self.remote
                                .as_ref()
                                .is_some_and(|remote| remote.state.is_playing == wanted)
                        {
                            self.optimistic_playing = None;
                        }
                        if uri != previous_uri {
                            self.on_now_playing_changed();
                        }
                    }
                    Err(error) => log::debug!("playback state unavailable: {error}"),
                }
            }
            ApiResponse::Queue { seq, result } => {
                if seq != self.queue_seq {
                    // A newer request supersedes this response.
                    return;
                }
                // Partial reads and read failures must preserve optimistic
                // additions while Spotify is still accepting their writes.
                let writing_queue = self.pending_queue_batches
                    .values()
                    .any(|target| *target == self.target());
                if writing_queue
                    || result
                        .as_ref()
                        .is_ok_and(|fetched| self.queue_fetch_is_stale(fetched))
                {
                    // Keep the optimistic queue and retry after a stale
                    // response. Accept Spotify's state after the retry limit.
                    self.queue_stale_retries = self.queue_stale_retries.saturating_add(1);
                    self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
                    return;
                }
                self.queue_stale_retries = 0;
                if result.is_ok() {
                    self.queue_cleared = None;
                    self.queue_shuffle_pending = None;
                    self.queue_reorder_pending = None;
                    self.queue_removed = None;
                }
                self.queue = Loadable::from_result(result);
                self.reconcile_pending_queue();
                if let Some(queue) = self.queue.get() {
                    let uris: Vec<String> = queue
                        .queue
                        .iter()
                        .map(|item| item.uri().to_string())
                        .collect();
                    self.request_contains(uris);
                }
            }
            ApiResponse::RecentlyPlayed {
                who,
                generation,
                limit,
                result,
            } => match who {
                RecentsFor::Home => {
                    if generation != self.home.generation {
                        return;
                    }
                    if let Ok(page) = &result {
                        self.note_recent_contexts(&page.items);
                    }
                    let items = result
                        .as_ref()
                        .map(|page| page.items.clone())
                        .map_err(|error| error.to_string());
                    let changed = items.is_ok();
                    self.home.refresh_failed |= !changed;
                    self.home.recently_played.refresh(items);
                    self.home_answered(changed);
                }
                RecentsFor::Panel => {
                    if generation != self.recents_generation {
                        return;
                    }
                    self.recents.loading = false;
                    self.recents.loaded_once = true;
                    match result {
                        Ok(page) => self.absorb_recents(page, limit),
                        Err(error) => self.recents.error = Some(error.to_string()),
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
                    if generation != self.home.top_songs_generation {
                        return;
                    }
                    match result {
                        Ok(page) => {
                            let received = page.items.len() as u32;
                            let tracks = page.items;
                            let uris: Vec<String> =
                                tracks.iter().map(|track| track.uri.clone()).collect();
                            self.request_contains(uris);
                            if offset == 0 {
                                self.home.top_songs = Loadable::Loaded(tracks);
                            } else if let Some(current) = self.home.top_songs.get_mut() {
                                current.extend(tracks);
                            }
                            if page.next.is_some() && received > 0 && offset + received < 100 {
                                self.backend.api(ApiRequest::TopTracks {
                                    offset: offset + received,
                                    full: true,
                                    generation,
                                });
                            } else {
                                self.home.top_songs_loading = false;
                                self.home.top_songs_complete = true;
                            }
                        }
                        Err(error) => {
                            self.home.top_songs.refresh(Err::<Vec<Track>, _>(error));
                            self.home.top_songs_loading = false;
                        }
                    }
                } else if generation != self.home.generation {
                } else if let Ok(page) = result {
                    let tracks = page.items;
                    let seeds: Vec<String> = tracks
                        .iter()
                        .filter_map(|track| track.id.clone())
                        .take(5)
                        .collect();
                    if !seeds.is_empty() {
                        if self.home.recommendations.get().is_none() {
                            self.home.recommendations = Loadable::Loading;
                        }
                        self.backend.api(ApiRequest::Recommendations {
                            seed_tracks: seeds,
                            seed_artists: Vec::new(),
                            generation,
                        });
                    }
                    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();
                    self.request_contains(uris);
                    self.home.top_tracks = Loadable::Loaded(tracks);
                    self.home_answered(true);
                } else if let Err(error) = result {
                    if offset == 0 && self.home.top_tracks.get().is_none() {
                        self.home.top_tracks = Loadable::Failed(error.to_string());
                    }
                    self.home.refresh_failed = true;
                    self.home_answered(false);
                }
            }
            ApiResponse::TopArtists { generation, result } => {
                if generation != self.home.generation {
                    return;
                }
                let changed = result.is_ok();
                self.home.refresh_failed |= !changed;
                self.home.top_artists.refresh(result);
                self.home_answered(changed);
            }
            ApiResponse::Recommendations { generation, result } => {
                if generation != self.home.generation {
                    return;
                }
                if let Ok(tracks) = &result {
                    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();
                    self.request_contains(uris);
                }
                self.home.recommendations.refresh(result);
            }
            ApiResponse::MadeForYou {
                generation,
                personalized,
                result,
            } => {
                if generation != self.home.made_for_you_generation {
                    return;
                }
                let changed = result.is_ok();
                self.home.refresh_failed |= !changed;
                if changed {
                    self.home.made_for_you_personalized = personalized;
                }
                self.home.made_for_you.refresh(result);
                if self.home.made_for_you_loading {
                    self.home.made_for_you_loading = false;
                    self.home_answered(changed);
                }
            }
            // A reload reads the playlists from the top again under a new
            // generation, so a page any earlier load asked for no longer
            // continues the list, whatever its offset. Within one load, only
            // the later page on its way is taken, and only once.
            ApiResponse::MyPlaylists {
                offset, generation, ..
            } if generation != self.library.playlists_generation
                || (offset > 0 && self.library.playlists_asked != Some(offset)) => {}
            ApiResponse::MyPlaylists { offset, result, .. } => match result {
                Ok(page) => {
                    self.library.playlists_asked = None;
                    let next_offset = page.next_offset();
                    let shown = match &mut self.library.playlists_incoming {
                        Some(incoming) => {
                            incoming.extend(page.items);
                            false
                        }
                        None => {
                            match &mut self.library.playlists {
                                Loadable::Loaded(existing) if offset > 0 => {
                                    existing.extend(page.items)
                                }
                                slot => *slot = Loadable::Loaded(page.items),
                            }
                            true
                        }
                    };
                    self.library.playlists_next = next_offset;
                    if next_offset.is_some() {
                        self.load_more(Page::Home);
                        if self.library.playlists_asked.is_none() {
                            // Offline, the next page was not asked for. The
                            // load ends with what is shown, and the network
                            // coming back reads the list again.
                            let library = &mut self.library;
                            library.playlists_incoming = None;
                            library.playlists_loading_since = None;
                            library.playlists_retry_at = Some(Instant::now());
                        }
                    } else {
                        let generation = self.library.playlists_generation;
                        let library = &mut self.library;
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
                        self.read_rootlist();
                    }
                    if shown || next_offset.is_none() {
                        self.apply_playlist_edits();
                        self.fill_blank_playlists();
                        self.note_listed_playlists();
                    }
                    if next_offset.is_none() {
                        self.store_library_cache();
                    }
                }
                Err(error) => {
                    let library = &mut self.library;
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
                        self.toast_error(
                            // Translators: {error} is an error message.
                            self.localize.t(self.locale, "Couldn't load more playlists: {error}")
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
                if self.playlist_pages
                    .get(&id)
                    .is_none_or(|page| page.generation != generation || page.pending_writes > 0)
                {
                    return;
                }
                let stale_write_snapshot = self.playlist_pages.get_mut(&id).is_some_and(|page| {
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
                    let page = self.playlist_pages.get_mut(&id).unwrap();
                    if page.snapshot_rechecks <= 3 {
                        self.backend.api(ApiRequest::Playlist { id, generation });
                    } else {
                        page.refresh_after_write = false;
                        page.items.fail(
                            self.localize.t(
                                self.locale,
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
                        playlist.owner.display_name = self.own_name(playlist.owner.id.as_deref());
                    }
                    // The owner's avatar comes from the profile view only
                    // the session can ask, so whoever owns the page shown
                    // is asked about once, next to the names already being
                    // resolved for the songs' adders.
                    if let Some(owner) = playlist.owner.id.clone().filter(|owner| !owner.is_empty())
                    {
                        self.request_user_names(vec![owner]);
                    }
                    if let Some(listed) = self.library_entry(&id) {
                        playlist.fill_from(listed);
                    }
                    if let Some(image) = pick_image(&playlist.images, 64) {
                        self.tint_for(Some(image));
                    }
                }
                let mut refresh_rows = false;
                if let Some(page) = self.playlist_pages.get_mut(&id) {
                    if page.refresh_after_write || page.optimistic_snapshot.is_some() {
                        match &result {
                            Ok(_) => refresh_rows = page.refresh_after_write,
                            Err(error) => {
                                page.refresh_after_write = false;
                                page.items.fail(friendly_page_error(self.localize.as_ref(), self.locale, error));
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
                self.try_adopt_playlist_cache(&id);
                self.checkpoint_playlist_cache(&id);
                if refresh_rows {
                    self.reload(Page::Playlist(id));
                }
            }
            ApiResponse::PlaylistItems {
                id,
                offset,
                generation,
                result,
            } => {
                if self.playlist_pages.get(&id).is_none_or(|page| {
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
                if let Some(page) = self.playlist_pages.get_mut(&id) {
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
                            note_availability(&mut self.playlist_availability, &items.items);
                            fill_availability(&self.playlist_availability, &mut items.items);
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
                                && self.table_rows.get(&cache_key).is_some_and(|cache| {
                                    cache.generation == generation
                                        && cache.items_revision == page.items.revision
                                        && cache.user_names_revision == self.user_names_revision
                                        && cache.playlist_positions.is_some()
                                        && cache.playlist_raw_count == page.items.items.len()
                                });
                            if !extends_checkpoint {
                                page.cache_append_valid = false;
                            }
                            page.items.absorb(offset, items);
                            page.items_generation = generation;
                            if extends_cached_rows
                                && let Some(cache) = self.table_rows.get_mut(&cache_key)
                            {
                                cache.playlist_append_revision = Some(page.items.revision);
                            }
                        }
                        Err(error) => page.items.fail(friendly_page_error(self.localize.as_ref(), self.locale, &error)),
                    }
                }
                for track in &tracks {
                    self.remember_track_recording(track);
                }
                self.request_contains(uris);
                self.request_user_names(adders);
                self.sample_playlist_tail(&id);
                self.checkpoint_playlist_cache(&id);
                // A sorted table means the whole list, not the loaded part.
                if self.table_sorts.contains_key(&Page::Playlist(id.clone())) {
                    self.load_more(Page::Playlist(id));
                }
            }
            ApiResponse::PlaylistSample {
                id,
                generation,
                result,
            } => {
                if self.playlist_pages
                    .get(&id)
                    .is_none_or(|page| page.generation != generation)
                {
                    return;
                }
                let mut adders: Vec<String> = Vec::new();
                if let Ok(items) = result
                    && let Some(page) = self.playlist_pages.get_mut(&id)
                {
                    adders = items
                        .items
                        .iter()
                        .filter_map(|item| item.added_by.as_ref()?.id.clone())
                        .filter(|id| !id.is_empty())
                        .collect();
                    page.contributors.extend(adders.iter().cloned());
                }
                self.request_user_names(adders);
            }
            ApiResponse::PlaylistCreated(result) => {
                self.playlist_busy = false;
                match result {
                    Ok(playlist) => {
                        self.toast(
                            // Translators: {name} is a playlist name.
                            self.localize.t(self.locale, "Created {name}")
                                .replace("{name}", &playlist.name),
                        );
                        let settled_by = self.library.playlists_generation + 1;
                        self.edit_playlists(PlaylistEdit {
                            uri: playlist.uri.clone(),
                            added: Some(playlist.clone()),
                            settled_by: Some(settled_by),
                        });
                        self.saved.insert(playlist.uri.clone(), true);
                        self.store_library_cache();
                        if let Some(Dialog::CreatePlaylist { add_uris, .. }) = self.dialog.take()
                            && !add_uris.is_empty()
                        {
                            self.backend.api(ApiRequest::AddToPlaylist {
                                playlist_id: playlist.id.clone(),
                                playlist_name: playlist.name.clone(),
                                uris: add_uris,
                                position: None,
                            });
                        }
                        self.open(Page::Playlist(playlist.id));
                    }
                    Err(error) => {
                        self.toast_error(
                            // Translators: {error} is an error message.
                            self.localize.t(self.locale, "Couldn't create the playlist: {error}")
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
                if self.cover_uploads.get(&id) != Some(&request) {
                    return;
                }
                self.cover_uploads.remove(&id);
                if let Some(Dialog::EditPlaylist {
                    id: current,
                    cover: draft,
                    ..
                }) = &mut self.dialog
                    && *current == id
                    && draft.uploading == Some(request)
                {
                    draft.uploading = None;
                    draft.error = result
                        .as_ref()
                        .err()
                        .map(|error| cover_error(self.localize.as_ref(), self.locale, error));
                    if result.is_ok() {
                        draft.selection = None;
                    }
                }
                match result {
                    Ok(()) => {
                        self.set_playlist_cover(&id, &cover);
                        self.uploaded_covers.insert(
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
                        if let Some(page) = self.playlist_pages.get(&id) {
                            self.backend.api(ApiRequest::Playlist {
                                id,
                                generation: page.generation,
                            });
                        }
                        self.toast(self.localize.t(self.locale, "Playlist cover updated"));
                    }
                    Err(error) => self.toast_error(cover_error(self.localize.as_ref(), self.locale, &error)),
                }
            }
            ApiResponse::PlaylistUpdated { id, result } => {
                self.playlist_busy = false;
                match result {
                    Ok(()) => {
                        self.toast(self.localize.t(self.locale, "Playlist updated"));
                        self.playlist_pages.remove(&id);
                        self.load_playlists();
                        if matches!(self.page(), Page::Playlist(current) if *current == id) {
                            self.ensure_loaded(Page::Playlist(id));
                        }
                    }
                    Err(error) => {
                        self.toast_error(
                            // Translators: {error} is an error message.
                            self.localize.t(self.locale, "Couldn't update the playlist: {error}")
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
                    self.add_to_playlist_now(playlist_id, playlist_name, items, position)
                }
                Ok(duplicate_uris) => {
                    self.playlist_busy = false;
                    self.dialog = Some(Dialog::ConfirmPlaylistDuplicates {
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
                    self.add_to_playlist_now(playlist_id, playlist_name, items, position);
                }
            },
            ApiResponse::PlaylistItemsChanged {
                id,
                message,
                result,
            } => {
                self.playlist_busy = false;
                if let Some(page) = self.playlist_pages.get_mut(&id) {
                    page.pending_writes = page.pending_writes.saturating_sub(1);
                }
                match result {
                    Ok(snapshot) => {
                        if !message.is_empty() {
                            self.toast(message);
                        }
                        let mut generation = None;
                        if let Some(page) = self.playlist_pages.get_mut(&id) {
                            if let Some(playlist) = page.playlist.get_mut() {
                                playlist.snapshot_id = snapshot.clone();
                            }
                            if snapshot.is_some() {
                                page.optimistic_snapshot = snapshot.clone();
                                page.snapshot_rechecks = 0;
                            }
                            generation = Some(page.generation);
                        }
                        if let Some(playlists) = self.library.playlists.get_mut() {
                            for playlist in playlists.iter_mut().filter(|p| p.id == id) {
                                playlist.snapshot_id = snapshot.clone();
                            }
                        }
                        self.checkpoint_playlist_cache(&id);
                        // Reconcile metadata in the background. The mutation
                        // response and local rows are enough to keep the page;
                        // no item page is discarded or downloaded again.
                        if let Some(generation) = generation {
                            self.backend.api(ApiRequest::Playlist { id, generation });
                        }
                    }
                    Err(error) => {
                        self.toast_error(
                            // Translators: {error} is an error message.
                            self.localize.t(self.locale, "Playlist change failed: {error}")
                                .replace("{error}", &error.to_string()),
                        );
                        if let Some(page) = self.playlist_pages.get_mut(&id) {
                            page.optimistic_snapshot = None;
                            page.snapshot_rechecks = 0;
                        }
                        self.load_playlist_items_at(&id, 0);
                        if let Some(page) = self.playlist_pages.get(&id) {
                            self.backend.api(ApiRequest::Playlist {
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
                    self.settle_playlist_edit(&format!("spotify:playlist:{id}"), true);
                    self.saved
                        .insert(format!("spotify:playlist:{id}"), followed);
                    self.toast(if followed {
                        self.localize.t(self.locale, "Added to Your Library")
                    } else {
                        self.localize.t(self.locale, "Removed from Your Library")
                    });
                    self.load_playlists();
                    if !followed && matches!(self.page(), Page::Playlist(current) if *current == id)
                    {
                        self.open(Page::Home);
                    }
                }
                Err(error) => {
                    self.settle_playlist_edit(&format!("spotify:playlist:{id}"), false);
                    self.saved
                        .insert(format!("spotify:playlist:{id}"), !followed);
                    self.toast_error(
                        // Translators: {error} is an error message.
                        self.localize.t(self.locale, "Couldn't update the playlist: {error}")
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
                if generation != self.liked_songs.generation
                    || self.user_id() != account_id.as_deref()
                {
                    return;
                }
                let mut more = false;
                let succeeded = result.is_ok();
                match result {
                    Ok(page) => {
                        for item in &page.items {
                            let uri = item.track.uri.clone();
                            self.remember_track_recording(&item.track);
                            if !self.saved_writes.contains_key(&uri)
                                && self.liked_songs.intent(&uri).is_none()
                            {
                                self.set_saved_state(uri, true);
                            }
                        }
                        more = self.liked_songs.absorb(
                            offset,
                            page,
                            jiff::Timestamp::now().as_second(),
                        );
                    }
                    Err(error) => self.liked_songs
                        .fail(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
                }
                if !more {
                    self.sync_liked_songs();
                    self.checkpoint_liked_songs(false);
                }
                if succeeded && (more || self.table_sorts.contains_key(&Page::LikedSongs)) {
                    self.load_more(Page::LikedSongs);
                }
            }
            // A reset shelf is read again from the top, so a page asked for
            // before the reset no longer continues it.
            ApiResponse::SavedAlbums { offset, .. }
                if self.library.albums.next_offset != Some(offset) => {}
            ApiResponse::SavedAlbums { offset, result } => match result {
                Ok(page) => {
                    self.request_album_types(page.items.iter().map(|item| &item.album));
                    for item in &page.items {
                        self.saved.insert(item.album.uri.clone(), true);
                    }
                    self.library.albums.absorb(offset, page);
                }
                Err(error) => self.library
                    .albums
                    .fail(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
            },
            ApiResponse::FollowedArtists { after, result } => {
                let list = &mut self.library.artists;
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
                            self.saved.insert(artist.uri.clone(), true);
                        }
                        list.items.extend(page.items);
                        let next = page.cursors.and_then(|cursors| cursors.after);
                        list.complete = next.is_none() || received == 0;
                        list.after = next;
                        list.error = None;
                    }
                    Err(error) => list.error = Some(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
                }
            }
            ApiResponse::SavedShows { offset, .. }
                if self.library.shows.next_offset != Some(offset) => {}
            ApiResponse::SavedShows { offset, result } => match result {
                Ok(page) => {
                    for item in &page.items {
                        self.saved.insert(item.show.uri.clone(), true);
                    }
                    let unknown: Vec<String> = page
                        .items
                        .iter()
                        .map(|item| item.show.uri.clone())
                        .filter(|uri| self.audiobooks_requested.insert(uri.clone()))
                        .collect();
                    if !unknown.is_empty() {
                        self.backend.send(Command::AudiobookShows(unknown));
                    }
                    self.library.shows.absorb(offset, page);
                    if offset == 0 && self.home.requested {
                        self.request_home_episodes();
                    }
                }
                Err(error) => self.library
                    .shows
                    .fail(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
            },
            ApiResponse::HomeEpisodes { generation, .. } if generation != self.home.generation => {}
            ApiResponse::HomeEpisodes { result, .. } => match result {
                Ok(podcasts) => {
                    self.home.podcasts = podcasts;
                    self.home.podcasts_answered = true;
                    self.store_home_cache();
                }
                // The shelf is an extra: without an answer it keeps what it
                // showed, or stays hidden.
                Err(error) => log::debug!("podcast episodes for Home unavailable: {error}"),
            },
            ApiResponse::SavedEpisodes { offset, .. }
                if self.library.episodes.next_offset != Some(offset) => {}
            ApiResponse::SavedEpisodes { offset, result } => match result {
                Ok(page) => {
                    for item in &page.items {
                        self.saved.insert(item.episode.uri.clone(), true);
                    }
                    self.library.episodes.absorb(offset, page);
                }
                Err(error) => self.library
                    .episodes
                    .fail(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
            },
            ApiResponse::SavedChanged {
                uris,
                saved,
                result,
            } => {
                let current_uris: Vec<String> = uris
                    .iter()
                    .filter(|uri| {
                        self.saved_writes
                            .get(*uri)
                            .is_none_or(|wanted| *wanted == saved)
                    })
                    .cloned()
                    .collect();
                for uri in &uris {
                    if self.saved_writes.get(uri) == Some(&saved) {
                        self.saved_writes.remove(uri);
                    }
                }
                match result {
                    Ok(()) => {
                        for uri in &current_uris {
                            self.set_saved_state(uri.clone(), saved);
                            match util::uri_kind(uri) {
                                Some("track") => {
                                    if self.liked_songs.intent(uri).is_some() {
                                        self.liked_songs.confirm(uri, saved, true);
                                    } else if !saved {
                                        self.library.liked.retain(|item| item.track.uri != *uri);
                                        if let Some(total) = self.library.liked.total.as_mut() {
                                            *total = total.saturating_sub(1);
                                        }
                                    }
                                }
                                Some("album") => self.library.albums.reset(),
                                Some("artist") => self.library.artists.reset(),
                                Some("show") => self.library.shows.reset(),
                                Some("episode") => self.library.episodes.reset(),
                                _ => {}
                            }
                        }
                        let message = match (
                            current_uris.first().and_then(|uri| util::uri_kind(uri)),
                            saved,
                        ) {
                            (Some("track"), true) => self.localize.t(self.locale, "Added to Liked Songs"),
                            (Some("track"), false) => {
                                self.localize.t(self.locale, "Removed from Liked Songs")
                            }
                            (Some("artist"), true) => self.localize.t(self.locale, "Following artist"),
                            (Some("artist"), false) => self.localize.t(self.locale, "Unfollowed artist"),
                            (_, true) => self.localize.t(self.locale, "Saved to Your Library"),
                            (_, false) => self.localize.t(self.locale, "Removed from Your Library"),
                        };
                        if !current_uris.is_empty() {
                            self.toast(message);
                        }
                    }
                    Err(error) => {
                        for uri in &current_uris {
                            self.set_saved_state(uri.clone(), !saved);
                            self.liked_songs.confirm(uri, saved, false);
                        }
                        if !current_uris.is_empty() {
                            self.toast_error(
                                // Translators: {error} is an error message.
                                self.localize.t(self.locale, "Couldn't update your library: {error}")
                                    .replace("{error}", &error.to_string()),
                            );
                        }
                    }
                }
                if self.liked_songs.cache_checked && self.liked_songs.has_confirmed_changes() {
                    self.sync_liked_songs();
                    self.checkpoint_liked_songs(true);
                    self.liked_recheck_at = Some(Instant::now() + Duration::from_secs(5));
                }
            }
            ApiResponse::Contains { uris, result } => {
                for uri in &uris {
                    self.saved_pending.remove(uri);
                }
                if let Ok(flags) = result {
                    for (uri, flag) in uris.into_iter().zip(flags) {
                        if !self.saved_writes.contains_key(&uri)
                            && self.liked_songs.intent(&uri).is_none()
                        {
                            self.set_saved_state(uri, flag);
                        }
                    }
                }
            }
            ApiResponse::SearchStarted {
                query,
                serial,
                split,
            } => {
                if serial == self.search.serial && query == self.search.committed {
                    self.search.catalogue_pending = true;
                    self.search.playlists_pending = split;
                }
            }
            ApiResponse::PlaySearch {
                query,
                serial,
                result,
            } => {
                if serial != self.play_search_serial {
                    // Superseded by a newer search or by something the
                    // user played meanwhile.
                    return;
                }
                match result {
                    Ok(Some(uri)) => self.actions.push(Action::PlaySearchMatch { uri, serial }),
                    Ok(None) => self.toast(
                        self.localize.t(
                            self.locale,
                            // Translators: {query} is the words someone searched for.
                            "Nothing on Spotify matches \u{201c}{query}\u{201d}",
                        )
                        .replace("{query}", &query),
                    ),
                    Err(error) => self.toast_error(
                        self.localize.t(
                            self.locale,
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
                if serial != self.search.serial || query != self.search.committed {
                    return;
                }
                self.search.playlists_pending = false;
                match result {
                    Ok(page) => {
                        self.search.playlists = Some((serial, page));
                        self.show_search_playlists();
                        self.remember_search(&query);
                    }
                    Err(error) => self.search_failed(&self.localize.t(self.locale, "Playlists"), error),
                }
            }
            ApiResponse::Search {
                query,
                serial,
                result,
            } => {
                if serial != self.search.serial || query != self.search.committed {
                    return;
                }
                self.search.catalogue_pending = false;
                match result {
                    Ok(results) => {
                        let uris = results
                            .tracks
                            .iter()
                            .flat_map(|page| page.items.iter())
                            .map(|track| track.uri.clone())
                            .collect();
                        self.request_contains(uris);
                        self.remember_search(&query);
                        self.search.results = Loadable::Loaded(results);
                        self.search.results_serial = serial;
                        self.show_search_playlists();
                    }
                    Err(error) => self.search_failed(&self.localize.t(self.locale, "Search"), error),
                }
            }
            ApiResponse::Artist { id, result } => {
                if let Ok(artist) = &result {
                    if let Some(image) = pick_image(&artist.images, 64) {
                        self.tint_for(Some(image));
                    }
                    if let Some(page) = self.artist_pages.get_mut(&id)
                        && page.top_tracks.needs_load()
                    {
                        page.top_tracks = Loadable::Loading;
                        self.backend
                            .api(ApiRequest::ArtistTopTracks { id: id.clone() });
                    }
                }
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    page.artist = match result {
                        Ok(artist) => Loadable::Loaded(artist),
                        Err(error) => Loadable::Failed(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
                    };
                }
            }
            ApiResponse::ArtistTopTracks { id, result } => {
                if let Ok(tracks) = &result {
                    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();
                    self.request_contains(uris);
                }
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    page.top_tracks = match result {
                        Ok(tracks) => Loadable::Loaded(tracks),
                        Err(error) => Loadable::Failed(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
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
                    self.request_album_types(albums.items.iter());
                }
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    let list = page.albums.entry(groups).or_default();
                    match result {
                        Ok(albums) => list.absorb(offset, albums),
                        Err(error) => list.fail(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
                    }
                }
            }
            ApiResponse::RelatedArtists { id, result } => {
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    page.related = match result {
                        Ok(artists) => Loadable::Loaded(artists),
                        Err(error) => Loadable::Failed(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
                    };
                }
            }
            ApiResponse::Album { id, result } => {
                // An earlier Web API header has no page generation. Once the
                // session resolves a countdown page, its preview and playable
                // cache own this album; a late generic header cannot replace it.
                if self.album_pages
                    .get(&id)
                    .is_some_and(|page| page.prerelease.is_some())
                {
                    return;
                }
                let mut uris = Vec::new();
                if let Ok(album) = &result {
                    self.request_album_types(std::iter::once(album));
                }
                if let Ok(album) = &result
                    && let Some(image) = pick_image(&album.images, 64)
                {
                    self.tint_for(Some(image));
                }
                if let Some(page) = self.album_pages.get_mut(&id) {
                    match result {
                        Ok(mut album) => {
                            if let Some(tracks) = album.tracks.take() {
                                uris = tracks.items.iter().map(|track| track.uri.clone()).collect();
                                page.tracks.absorb(0, tracks);
                            }
                            page.album = Loadable::Loaded(album);
                            if !page.tracks.loaded_once {
                                page.tracks.loading = true;
                                self.backend.api(ApiRequest::AlbumTracks {
                                    id,
                                    offset: 0,
                                    generation: page.generation,
                                });
                            }
                        }
                        Err(error) => {
                            page.album = Loadable::Failed(friendly_api_error(self.localize.as_ref(), self.locale, &error))
                        }
                    }
                }
                self.request_contains(uris);
            }
            ApiResponse::AlbumTracks {
                id,
                offset,
                generation,
                result,
            } => {
                if self.album_pages
                    .get(&id)
                    .is_none_or(|page| page.generation != generation)
                {
                    return;
                }
                let mut uris = Vec::new();
                if let Some(page) = self.album_pages.get_mut(&id) {
                    match result {
                        Ok(tracks) => {
                            uris = tracks.items.iter().map(|track| track.uri.clone()).collect();
                            page.tracks.absorb(offset, tracks);
                        }
                        Err(error) => page.tracks.fail(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
                    }
                }
                self.request_contains(uris);
                // A sorted table means the whole list, not the loaded part.
                if self.table_sorts.contains_key(&Page::Album(id.clone())) {
                    self.load_more(Page::Album(id));
                }
            }
            ApiResponse::AlbumQueueTracks {
                request,
                offset,
                result,
            } => {
                self.receive_album_queue(request, offset, result);
            }
            ApiResponse::Show { id, result } => {
                if let Ok(show) = &result
                    && let Some(image) = pick_image(&show.images, 64)
                {
                    self.tint_for(Some(image));
                }
                if let Some(page) = self.show_pages.get_mut(&id) {
                    match result {
                        Ok(mut show) => {
                            if let Some(episodes) = show.episodes.take() {
                                page.episodes.absorb(0, episodes);
                            }
                            page.show = Loadable::Loaded(show);
                            if !page.episodes.loaded_once {
                                page.episodes.loading = true;
                                self.backend.api(ApiRequest::ShowEpisodes { id, offset: 0 });
                            }
                        }
                        Err(error) => {
                            page.show = Loadable::Failed(friendly_api_error(self.localize.as_ref(), self.locale, &error))
                        }
                    }
                }
            }
            ApiResponse::ShowEpisodes { id, offset, result } => {
                if let Some(page) = self.show_pages.get_mut(&id) {
                    match result {
                        Ok(episodes) => page.episodes.absorb(offset, episodes),
                        Err(error) => page.episodes.fail(friendly_api_error(self.localize.as_ref(), self.locale, &error)),
                    }
                }
            }
            ApiResponse::Track { id, result } => {
                self.track_requests.remove(&id);
                match result {
                    Ok(track) => {
                        self.remember_track_recording(&track);
                        self.request_recording_candidates(&track);
                        if self.liked_songs.update_track(&track) {
                            self.sync_liked_songs();
                        }
                        self.resolve_pasted_song(
                            &format!("spotify:track:{id}"),
                            Some(PlayableItem::Track(track.clone())),
                        );
                        self.track_cache.insert(id.clone(), track);
                        self.track_used.insert(id, Instant::now());
                    }
                    Err(error) => {
                        self.resolve_pasted_song(&format!("spotify:track:{id}"), None);
                        if self.pending_link.as_deref()
                            == Some(format!("spotify:track:{id}").as_str())
                        {
                            self.pending_link = None;
                            self.toast_error(
                                // Translators: {error} is an error message.
                                self.localize.t(self.locale, "Cannot open this song: {error}")
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
                    Some(show) => self.open(Page::Show(show.id)),
                    None => self.toast_error(self.localize.t(
                        self.locale,
                        "This episode's podcast is not on Spotify",
                    )),
                },
                Err(error) => self.toast_error(
                    // Translators: {error} is an error message.
                    self.localize.t(self.locale, "Cannot open this episode: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            ApiResponse::Remote { action, result } => {
                if matches!(action, RemoteAction::Play | RemoteAction::Pause) {
                    self.clear_play_pending();
                }
                match result {
                    Ok(()) => {
                        self.remote_recheck_at = Some(Instant::now() + REMOTE_RECHECK);
                        self.remote_command_at = Some(Instant::now());
                        if action == RemoteAction::Shuffle {
                            self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
                            if let Some(pending) = &mut self.queue_shuffle_pending {
                                pending.at = Instant::now();
                            } else {
                                self.note_shuffle_pending();
                            }
                        }
                    }
                    Err(error) => {
                        self.optimistic_playing = None;
                        self.pending_remote_position = None;
                        self.pending_remote_volume = None;
                        if matches!(
                            action,
                            RemoteAction::Play | RemoteAction::Next | RemoteAction::Previous
                        ) && self.intent_track.as_ref().is_some_and(|intent| {
                            matches!(intent.confirmation, TrackConfirmation::Remote { .. })
                        }) {
                            self.intent_track = None;
                        }
                        let hint = if error.status() == Some(404) {
                            format!(
                                " {}",
                                self.localize.t(
                                    self.locale,
                                    "Choose a device from the devices menu first."
                                )
                            )
                        } else {
                            String::new()
                        };
                        self.toast_error(format!(
                            "{}: {error}.{hint}",
                            remote_action_label(self.localize.as_ref(), self.locale, action)
                        ));
                    }
                }
                self.poll_remote_soon();
            }
            ApiResponse::Transferred { device_id, result } => match result {
                Ok(()) => {
                    self.selected_device = Some(device_id);
                    self.show_devices = false;
                    self.remote_command_at = Some(Instant::now());
                    self.poll_remote_soon();
                    self.refresh_devices();
                }
                Err(error) => self.toast_error(
                    // Translators: {error} is an error message.
                    self.localize.t(self.locale, "Couldn't switch device: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            ApiResponse::QueueAdded { label: _, result } => match result {
                Ok(()) => {
                    // Refresh the queue after the optimistic update.
                    self.refresh_queue(true);
                }
                Err(error) => self.toast_error(
                    // Translators: {error} is an error message.
                    self.localize.t(self.locale, "Couldn't add to queue: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            ApiResponse::QueueBatchAdded {
                request,
                added,
                result,
            } => {
                if self.pending_queue_batches.remove(&request).as_ref() != Some(&self.target()) {
                    return;
                }
                if result.is_err() {
                    self.remove_failed_queue_adds(request, added);
                }
                for addition in &mut self.pending_queue_adds {
                    if addition.write.is_some_and(|(write, _)| write == request) {
                        addition.write = None;
                        addition.at = Instant::now();
                    }
                }
                if let Err(error) = result {
                    self.toast_error(
                        // Translators: {error} is an error message.
                        self.localize.t(self.locale, "Couldn't add to queue: {error}")
                            .replace("{error}", &error.to_string()),
                    );
                }
                self.refresh_queue(true);
            }
        }
    }
}

impl Engine {
    pub fn on_now_playing_changed(&mut self) {
        // Queue consumption and saved session state follow confirmed playback,
        // not the song shown optimistically while a command is still pending.
        let Some(now) = self.now_playing_live() else {
            return;
        };
        let same_uri = self.last_now_playing_uri.as_deref() == Some(now.uri.as_str());
        let new_occurrence =
            now.local && self.last_now_playing_sequence != self.local.track_sequence;
        if same_uri && !new_occurrence {
            return;
        }
        // A new track is a new resume point.
        self.session_dirty = true;
        let queue_already_updated =
            self.queue_start_pending.take().as_ref() == Some(&self.target());
        let repeating = same_uri && new_occurrence && now.repeat == RepeatMode::Track;
        // Restore the saved queue only when the remembered track resumes.
        if !self.resume_queue.is_empty() {
            let queued = std::mem::take(&mut self.resume_queue);
            self.session_dirty = true;
            if now.local && self.resume_track.as_deref() == Some(now.uri.as_str()) {
                for uri in queued {
                    self.manual_queue.push(uri.clone());
                    self.backend.api(ApiRequest::AddToQueue {
                        uri,
                        device_id: self.local_device_id.clone(),
                        label: String::new(),
                    });
                }
            }
        }
        // A context load can start the same song as the manual queue's head
        // without consuming that queued copy. Explicit skips have already
        // consumed their rows; only an unrequested advance consumes one here.
        if !queue_already_updated && !repeating {
            self.consume_manual_queue_head(&now.uri);
        }
        if !queue_already_updated
            && !repeating
            && let Loadable::Loaded(queue) = &mut self.queue
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
        self.last_now_playing_uri = Some(now.uri.clone());
        if now.local {
            self.last_now_playing_sequence = self.local.track_sequence;
        }
        self.resume_context = self.playing_context_uri();
        self.resume_track = Some(now.uri.clone());
        self.resume_position_ms = 0;
        if now.local
            && !now.is_episode
            && let Some(id) = &now.id
            && !self.track_cache.contains_key(id)
            && self.track_requests.insert(id.clone())
        {
            self.backend.api(ApiRequest::Track { id: id.clone() });
        }
        self.request_contains(vec![now.uri.clone()]);
        if let Some(url) = now.art_small.or(now.art_url) {
            self.tint_for(Some(&url));
        }
        if matches!(self.page(), Page::Queue)
            || self.show_queue_panel
            || (self.settings.winamp_window && self.settings.playlist_open)
        {
            self.refresh_queue(true);
        }
        if self.show_lyrics_panel {
            self.request_lyrics();
        }
    }
}

impl Engine {
    pub fn request_lyrics(&mut self) {
        let Some(now) = self.now_playing() else {
            return;
        };
        if self.lyrics_uri.as_deref() == Some(now.uri.as_str())
            && !matches!(self.lyrics, Loadable::NotLoaded | Loadable::Failed(_))
        {
            return;
        }
        self.lyrics_uri = Some(now.uri.clone());
        self.lyrics_following = true;
        self.lyrics_line_shown = None;
        if now.is_episode || self.offline {
            self.lyrics = Loadable::Loaded(None);
            return;
        }
        self.lyrics = Loadable::Loading;
        self.backend.send(Command::Lyrics(Box::new(LyricsRequest {
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
}

impl Engine {
    pub fn handle_event(&mut self, event: Event) {
        match event {
                Event::PlaylistCoverChecked {
                    id,
                    request,
                    images,
                    result,
                } => {
                    self.cover_checked(&id, request, images, result);
                }
                Event::PlaylistCoverChosen {
                    id,
                    request,
                    result,
                } => {
                    self.cover_chosen(&id, request, result);
                }
                Event::Auth(status) => self.handle_auth(status),
                Event::Playback(status) => self.handle_playback(status),
                Event::NetworkStatus(reachable) => {
                    let became_reachable = !self.network_reachable && reachable;
                    self.network_reachable = reachable;
                    if became_reachable {
                        self.on_network_restored();
                    }
                }
                Event::Receivers(receivers) => self.receivers = receivers,
                Event::ReceiverActivated { name, result } => {
                    self.activating_receiver = None;
                    match result {
                        Ok(()) => {
                            self.toast(
                                // Translators: {name} is the name of a speaker or other playback device.
                                self.localize.t(self.locale, "{name} is ready").replace("{name}", &name),
                            );
                            // It takes a moment to appear in the device list.
                            self.pending_transfer_to = Some((name, Instant::now()));
                            self.devices_fetched_at = None;
                            self.refresh_devices();
                        }
                        Err(error) => self.toast_error(format!("{name}: {error}")),
                    }
                }
                Event::Local(state) => self.handle_local(*state),
                Event::Api(response) => self.handle_api(*response),
                Event::Accent { url, color } => {
                    self.accent_pending.remove(&url);
                    self.accents.insert(url, color);
                }
                Event::ProxyRestored { config, password } => {
                    self.handle_proxy_restored(config, password)
                }
                Event::ProxyStorageFailed(error) => {
                    let message = if self.settings.proxy_password_legacy {
                        self.localize.t(
                            self.locale,
                            // Translators: {error} is a sentence saying why the proxy password could not be stored.
                            "{error} The original settings file is kept intact; changed preferences are not saved yet.",
                        )
                        .replace("{error}", error.proxy_message())
                    } else {
                        error.proxy_message().to_string()
                    };
                    self.toast_error(message);
                }
                Event::Error(message) => self.toast_error(message),
                Event::Rootlist { result } => match result {
                    Ok(rootlist) => {
                        let account_id = self.user_id().map(str::to_owned).or_else(|| {
                            if let AuthStatus::Connected { username } = &self.auth {
                                Some(username.clone())
                            } else {
                                None
                            }
                        });
                        self.rootlist = rootlist.entries;
                        self.editable_by_grant = rootlist.editable;
                        if let Some(account_id) = account_id {
                            self.rootlist_cache = Some(CachedRootlist {
                                account_id,
                                entries: self.rootlist.clone(),
                            });
                            self.session_dirty = true;
                        }
                    }
                    Err(error) => log::warn!("rootlist unavailable: {error}"),
                },
                Event::Lyrics { uri, result } => {
                    if self.lyrics_uri.as_deref() == Some(uri.as_str()) {
                        self.lyrics = match result {
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
                    self.receive_playlist_cache(&account_id, &id, generation, cache);
                }
                Event::PlaylistCacheStored {
                    account_id,
                    id,
                    generation,
                    snapshot,
                    success,
                } => {
                    self.receive_playlist_cache_stored(
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
                    self.receive_liked_cache(&account_id, generation, cache);
                }
                Event::AccountCaches {
                    account_id,
                    generation,
                    playlists,
                    home,
                } => {
                    self.receive_account_caches(&account_id, generation, playlists, home);
                }
                Event::UserName { id, name, image } => {
                    self.set_user_name(id.clone(), name);
                    self.set_user_image(id, image);
                }
                Event::AudiobookShows(uris) => {
                    self.audiobook_shows.extend(uris);
                }
                Event::PlaylistHeader(header) => {
                    self.playlist_headers.insert(header.id.clone(), *header);
                    self.fill_blank_playlists();
                }
                Event::Radio {
                    seed,
                    generation,
                    result,
                } => self.receive_radio(&seed, generation, result),
                Event::Prerelease {
                    uri,
                    generation,
                    result,
                } => self.receive_prerelease(&uri, generation, result.map(|data| *data)),
                Event::PrereleaseSaved {
                    uri,
                    saved,
                    generation,
                    result,
                } => self.receive_prerelease_saved(&uri, saved, generation, result),
                Event::AlbumType { uri, result } => match result {
                    Ok(true) => {
                        self.confirmed_ep_albums.insert(uri);
                    }
                    Ok(false) => {}
                    Err(error) => log::debug!("album type unavailable for {uri}: {error}"),
                },
                Event::WebApp { client_id } => self.web_app = client_id,
                _ => {}
        }
    }
}

impl Engine {
    pub fn apply(&mut self, action: Action) {
        match action {

            Action::Open(page) => self.open(page),
            Action::OpenSongRadio { uri, track } => self.open_song_radio(&uri, &track),
            Action::PrepareTint(url) => {
                if self.settings.accent_from_art {
                    self.tint_for(Some(&url));
                }
            }
            Action::OpenUri(uri) => {
                if let Some(page) = Page::from_uri(&uri) {
                    self.open(page);
                }
            }
            Action::OpenLink(uri) => {
                // The window first: a link is the user asking for the
                // app, signed in or not, and the page follows when it can.
                self.prerelease_request = None;
                self.pending_link = Some(uri);
                self.open_pending_link();
                self.actions.push(Action::ShowWindow);
            }
            Action::Back => {
                if self.can_go_back() {
                    self.pending_link = None;
                    self.prerelease_request = None;
                    self.history_index -= 1;
                    let page = self.page().clone();
                    self.touch_page(&page);
                    self.ensure_loaded(page.clone());
                    self.retain_table_rows(&page);
                    self.evict_stale_pages();
                }
            }
            Action::Forward => {
                if self.can_go_forward() {
                    self.pending_link = None;
                    self.prerelease_request = None;
                    self.history_index += 1;
                    let page = self.page().clone();
                    self.touch_page(&page);
                    self.ensure_loaded(page.clone());
                    self.retain_table_rows(&page);
                    self.evict_stale_pages();
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
                self.play_request(request, false);
            }
            // Answered by the search's response, which plays the match.
            // Taking the serial here, not when the command arrives, keeps
            // a play queued before it from superseding it.
            Action::PlaySearch { kind, query } => {
                self.play_search_serial = self.play_search_serial.wrapping_add(1);
                self.backend.api(ApiRequest::PlaySearch {
                    kind,
                    query,
                    serial: self.play_search_serial,
                });
            }
            Action::PlaySearchMatch { uri, serial } => {
                if serial == self.play_search_serial {
                    self.play_request(PlayRequest::context(uri), false);
                }
            }
            Action::PlayUris { uris, index } => {
                if uris.is_empty() {
                    return;
                }
                let (uris, index) = cap_uris(&uris, index);
                let request = PlayRequest::tracks(uris).starting_at_index(index);
                self.play_request(request, false);
            }
            Action::PlayEpisode { uri, resume_ms } => {
                // A started episode continues from the place the row or card
                // showed as time left. The playing episode is left where it
                // is playing, not sent back to a saved position older than it.
                let mut request = PlayRequest::tracks(vec![uri.clone()]).starting_at_index(0);
                if self.now_playing().is_none_or(|now| now.uri != uri)
                    && let Some(position_ms) = resume_ms
                {
                    request.position_ms = position_ms;
                }
                self.play_request(request, false);
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
                    self.play_request(request, false);
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
                    self.play_request(request, false);
                }
                RowContext::Queue => self.play_queue_item(index as usize, uri),
                RowContext::View {
                    uris, context_uri, ..
                } => {
                    let (uris, index) = cap_uris(uris.as_ref(), index);
                    if let Some(uri) = uris.get(index as usize) {
                        self.cache_track_from_context(&context_uri, uri);
                    }
                    let request = PlayRequest::tracks(uris).starting_at_index(index);
                    self.play_request(request, false);
                    self.note_recent_context(&context_uri);
                    self.assumed_context = Some(AssumedContext {
                        uri: context_uri,
                        shuffle: self.shuffle_wanted.then_some(true),
                        at: Instant::now(),
                    });
                }
            },
            Action::ShufflePlay(uri) => {
                // The random starting song is picked in `play_request`,
                // which every shuffled play goes through.
                self.play_request(PlayRequest::context(uri), true);
            }
            Action::TogglePlay => self.toggle_play(),
            Action::Next if self.resume_only() => {
                self.step_resume(true);
            }
            Action::Next => {
                // Move the queue head to the playing row immediately. Do not
                // pop when there is no active playback target.
                let target = self.target();
                if !matches!(target, Target::Remote(None)) || self.remote_fresh().is_some() {
                    self.pop_queue_head();
                }
                match target {
                    Target::Local => self.backend.player(PlayerCommand::Next),
                    Target::Remote(device_id) => self.remote(RemoteAction::Next, device_id),
                }
            }
            // Previous restarts after three seconds and otherwise steps back,
            // matching librespot.
            Action::Previous if self.resume_only() => {
                if self.resume_position_ms > RESTART_BEFORE_PREVIOUS {
                    self.resume_position_ms = 0;
                    self.session_dirty = true;
                } else {
                    self.step_resume(false);
                }
            }
            Action::Previous => {
                // Next names its expected destination so the row moves before
                // the engine does. Previous has no forward queue row to name;
                // discard that expectation or a quick Next, Previous leaves
                // the row marker stuck on the skipped-to song.
                self.intent_track = None;
                self.queue_start_pending = Some(self.target());
                match self.target() {
                    Target::Local => self.backend.player(PlayerCommand::Previous),
                    Target::Remote(device_id) => self.remote(RemoteAction::Previous, device_id),
                }
            }
            Action::Seek(position_ms) => self.seek(position_ms),
            Action::SeekBy(offset) => {
                if let Some(now) = self.now_playing() {
                    let target = (i64::from(now.position_ms) + offset)
                        .clamp(0, i64::from(now.duration_ms))
                        as u32;
                    self.seek(target);
                }
            }
            Action::SetVolume(percent) => {
                self.volume_before_mute = None;
                self.set_volume(percent, true);
            }
            Action::PreviewVolume(percent) => self.set_volume(percent, false),
            Action::VolumeBy(delta) => {
                if let Some(now) = self.now_playing() {
                    let next =
                        (i16::from(now.volume_percent) + i16::from(delta)).clamp(0, 100) as u8;
                    self.volume_before_mute = None;
                    self.set_volume(next, true);
                } else if self.is_connected() {
                    let current = volume_to_percent(self.local.volume);
                    let next = (i16::from(current) + i16::from(delta)).clamp(0, 100) as u8;
                    self.set_volume(next, true);
                }
            }
            Action::ToggleMute => {
                if !self.can_set_volume() {
                    return;
                }
                let current = self.now_playing()
                    .map(|now| now.volume_percent)
                    .unwrap_or_else(|| volume_to_percent(self.local.volume));
                if current == 0 {
                    let restore = self.volume_before_mute.take().unwrap_or(50).max(5);
                    self.set_volume(restore, true);
                } else {
                    self.volume_before_mute = Some(current);
                    self.set_volume(0, true);
                }
            }
            Action::ToggleShuffle => {
                let shuffle = self.now_playing()
                    .map_or(self.shuffle_wanted, |now| now.shuffle);
                self.set_shuffle(!shuffle);
            }
            Action::SetShuffle(shuffle) => self.set_shuffle(shuffle),
            Action::CycleRepeat => {
                let mode = self.now_playing().map(|now| now.repeat).unwrap_or_default();
                self.set_repeat(mode.next());
            }
            Action::SetRepeat(mode) => self.set_repeat(mode),
            Action::AddToQueue { uri, label } => self.add_to_queue(uri, label),
            Action::QueueMany { songs } => self.queue_many(songs),
            Action::MoveInQueue { from, to } => {
                if !self.queue_locally_reorderable() {
                    return;
                }
                let queued_len = self.queued_rows_len();
                if from >= queued_len || to > queued_len || from == to || to == from + 1 {
                    return;
                }
                if from < self.manual_queue.len() {
                    let uri = self.manual_queue.remove(from);
                    let at = (if to > from { to - 1 } else { to }).min(self.manual_queue.len());
                    self.manual_queue.insert(at, uri);
                    // Pending additions are tracked by manual_queue index;
                    // shift them the same way the move just shifted the row
                    // they point at, or they end up naming a different song.
                    for addition in &mut self.pending_queue_adds {
                        if addition.manual_index == from {
                            addition.manual_index = at;
                        } else if from < addition.manual_index && addition.manual_index <= at {
                            addition.manual_index -= 1;
                        } else if at <= addition.manual_index && addition.manual_index < from {
                            addition.manual_index += 1;
                        }
                    }
                    self.show_manual_queue_order(queued_len);
                }
                self.session_dirty = true;
                self.resync_local_queue();
            }
            Action::RemoveFromQueue { rows } => self.remove_from_queue(rows),
            Action::InsertInQueue { items, position } => {
                if !self.queue_locally_reorderable() {
                    let songs = items
                        .into_iter()
                        .map(|item| (item.uri().to_string(), item.name().to_string()))
                        .collect();
                    self.queue_many(songs);
                    return;
                }
                let position = position.min(self.queued_rows_len());
                let repeats: Vec<bool> = items
                    .iter()
                    .map(|item| self.queued_moments_ago(item.uri()))
                    .collect();
                let mut inserted = 0;
                for (item, repeat) in items.into_iter().zip(repeats) {
                    if repeat {
                        continue;
                    }
                    let uri = item.uri().to_string();
                    let at = (position + inserted).min(self.manual_queue.len());
                    // Inserting here shifts every later manual_queue row
                    // right by one; keep pending additions pointing at their
                    // own song rather than the one now sitting in their slot.
                    for addition in &mut self.pending_queue_adds {
                        if addition.manual_index >= at {
                            addition.manual_index += 1;
                        }
                    }
                    self.pending_queue_adds.push(PendingQueueAdd {
                        item: item.clone(),
                        at: Instant::now(),
                        manual_index: at,
                        write: None,
                    });
                    if let Loadable::Loaded(queue) = &mut self.queue {
                        queue.queue.insert(at.min(queue.queue.len()), item);
                    }
                    self.manual_queue.insert(at, uri);
                    inserted += 1;
                }
                if inserted > 0 {
                    self.session_dirty = true;
                    self.queued_toast(inserted);
                    self.resync_local_queue();
                }
            }
            Action::SetSavedMany { uris, saved } => {
                let mut changed_tracks = false;
                for uri in &uris {
                    self.set_saved_state(uri.clone(), saved);
                    self.saved_writes.insert(uri.clone(), saved);
                    changed_tracks |= self.change_liked_song(uri, saved);
                }
                if changed_tracks {
                    self.sync_liked_songs();
                    self.ensure_liked_songs();
                }
                if !uris.is_empty() {
                    self.backend.api(ApiRequest::SetSaved { uris, saved });
                }
            }
            Action::ToggleSaved(uri) => {
                let saved = self.is_saved(&uri).unwrap_or(false);
                let targets = if saved {
                    self.saved_toggle_targets(&uri)
                } else {
                    vec![uri]
                };
                for target in targets {
                    self.set_saved(target, !saved);
                }
            }
            Action::AddToPlaylist {
                playlist_id,
                playlist_name,
                items,
            } => {
                self.request_playlist_add(playlist_id, playlist_name, items, None);
            }
            Action::InsertInPlaylist {
                playlist_id,
                position,
                items,
            } => {
                let name = self.playlist_pages
                    .get(&playlist_id)
                    .and_then(|page| page.playlist.get())
                    .filter(|playlist| self.can_edit_playlist(playlist))
                    .map(|playlist| playlist.name.clone());
                if let Some(name) = name {
                    self.request_playlist_add(playlist_id, name, items, Some(position));
                }
            }
            Action::ConfirmAddToPlaylist {
                playlist_id,
                playlist_name,
                items,
                position,
            } => {
                self.add_to_playlist_now(playlist_id, playlist_name, items, position);
            }
            Action::RemoveFromPlaylist { playlist_id, uris } => {
                let snapshot_id = self.playlist_pages
                    .get(&playlist_id)
                    .and_then(|page| page.playlist.get())
                    .and_then(|playlist| playlist.snapshot_id.clone());
                self.prepare_playlist_mutation(&playlist_id);
                if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
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
                self.playlist_busy = true;
                self.backend.api(ApiRequest::RemoveFromPlaylist {
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
                let snapshot_id = self.playlist_pages
                    .get(&playlist_id)
                    .and_then(|page| page.playlist.get())
                    .and_then(|playlist| playlist.snapshot_id.clone());
                self.prepare_playlist_mutation(&playlist_id);
                if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
                    let base = page.items.base_offset;
                    if from >= base && to >= base {
                        page.items
                            .reorder((from - base) as usize, (to - base) as usize);
                    }
                }
                self.playlist_busy = true;
                self.backend.api(ApiRequest::ReorderPlaylist {
                    playlist_id,
                    range_start: from,
                    insert_before: to,
                    snapshot_id,
                });
            }
            Action::UploadPlaylistCover(id) => {
                if self.cover_uploads.contains_key(&id) {
                    return;
                }
                let previous_urls = self.uploaded_covers
                    .get(&id)
                    .map(|pending| pending.previous_urls.clone())
                    .unwrap_or_else(|| {
                        self.playlist_pages
                            .get(&id)
                            .and_then(|page| page.playlist.get())
                            .into_iter()
                            .chain(
                                self.library
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
                }) = &mut self.dialog
                    && *current == id
                    && cover.uploading.is_none()
                    && cover.request.is_none()
                    && let Some(selected) = cover.selection.clone()
                {
                    self.cover_request = self.cover_request.wrapping_add(1);
                    cover.uploading = Some(self.cover_request);
                    self.cover_uploads.insert(id.clone(), self.cover_request);
                    cover.error = None;
                    self.backend.api(ApiRequest::UploadPlaylistCover {
                        id,
                        request: self.cover_request,
                        previous_urls,
                        cover: selected,
                    });
                }
            }
            Action::ShowDialog(mut dialog) => {
                if let Dialog::EditPlaylist { id, cover, .. } = &mut dialog
                    && let Some(request) = self.cover_uploads.get(id)
                {
                    cover.uploading = Some(*request);
                }
                self.dialog = Some(dialog);
            }
            Action::CloseDialog => {
                if matches!(self.dialog, Some(Dialog::PersonalAppIntro)) {
                    self.settings.personal_app_intro_seen = true;
                    self.settings_dirty = true;
                }
                self.dialog = None;
            }
            Action::CreatePlaylist {
                name,
                public,
                add_uris,
            } => {
                self.playlist_busy = true;
                self.dialog = Some(Dialog::CreatePlaylist {
                    name: name.clone(),
                    public,
                    add_uris,
                });
                self.backend.api(ApiRequest::CreatePlaylist {
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
                self.dialog = None;
                let changes = self.changed_playlist_details(&id, name, description, public);
                if changes.kept_description {
                    self.toast_error(self.localize.t(
                        self.locale,
                        "Spotify doesn't let apps remove a playlist description, so it was kept",
                    ));
                }
                if changes.name.is_some()
                    || changes.description.is_some()
                    || changes.public.is_some()
                {
                    self.playlist_busy = true;
                    self.backend.api(ApiRequest::UpdatePlaylist {
                        id,
                        name: changes.name,
                        description: changes.description,
                        public: changes.public,
                    });
                }
            }
            Action::DeletePlaylist(id) => {
                self.dialog = None;
                let uri = format!("spotify:playlist:{id}");
                self.saved.insert(uri.clone(), false);
                self.edit_playlists(PlaylistEdit {
                    uri,
                    added: None,
                    settled_by: None,
                });
                self.backend
                    .api(ApiRequest::FollowPlaylist { id, follow: false });
            }
            Action::Transfer(device_id) => self.transfer(device_id),
            Action::ActivateReceiver(receiver) => {
                if self.activating_receiver.is_none() {
                    self.activating_receiver = Some(receiver.name.clone());
                    self.activating_play = self.now_playing().is_some_and(|now| now.playing);
                    self.backend.send(Command::ActivateReceiver(receiver));
                }
            }
            Action::RefreshDevices => {
                self.devices_fetched_at = None;
                self.refresh_devices();
                self.backend.send(Command::DiscoverReceivers);
            }
            Action::ClearQueue => self.clear_queue(),
            Action::SaveQueueAsPlaylist => self.save_queue_as_playlist(),
            Action::SaveRadio(seed) => self.save_radio(&seed),
            Action::RefreshQueue => self.refresh_queue(true),
            Action::PasteSongs { playlist_id, text } => self.paste_songs(playlist_id, &text),
            Action::Search(query) => {
                // Enter submits what was typed; a recent search or a link
                // starts afresh.
                let typed = query == self.search.query;
                self.search.query = query.clone();
                self.search.typed_at = None;
                self.open(Page::Search);
                if typed {
                    self.submit_typed_search(query.trim().to_string());
                } else {
                    self.search.edit_run += 1;
                    self.run_search(query.trim().to_string());
                }
            }
            Action::EndSearchRun => self.search.edit_run += 1,
            Action::ForgetSearch(query) => {
                self.settings.search_history.retain(|entry| entry != &query);
                self.settings_dirty = true;
            }
            Action::SetSearchFilter(filter) => self.search.filter = filter,
            Action::FocusSearch => {
                self.search.focus_requested = true;
                if !matches!(self.page(), Page::Search) {
                    self.open(Page::Search);
                }
            }
            Action::LoadMore(page) => self.load_more(page),
            Action::LoadWindow { page, position } => self.load_window(page, position),
            Action::RetryWindow(page) => self.retry_window(page),
            Action::LoadMoreRecents => self.load_more_recents(),
            Action::ReloadRecents => self.reload_recents(),
            Action::SetQueueTab(tab) => {
                self.queue_tab = tab;
                self.session_dirty = true;
                if tab == QueueTab::Recents
                    && self.recents.items.is_empty()
                    && !self.recents.loading
                {
                    self.load_recents(false);
                }
            }
            Action::LoadMoreArtistAlbums(id) => {
                let Some(page) = self.artist_pages.get_mut(&id) else {
                    return;
                };
                let groups = page.filter.groups().to_string();
                let list = page.albums.entry(groups.clone()).or_default();
                if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                    list.loading = true;
                    self.backend
                        .api(ApiRequest::ArtistAlbums { id, groups, offset });
                }
            }
            Action::SetDiscographyFilter { artist_id, filter } => {
                if let Some(page) = self.artist_pages.get_mut(&artist_id) {
                    page.filter = filter;
                }
                self.load_artist_albums(&artist_id, filter);
            }
            Action::ToggleShowAllTop(id) => {
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    page.show_all_top = !page.show_all_top;
                }
            }
            Action::Reload(page) => self.reload(page),
            Action::ProxyEdited => self.proxy_form_edited = true,
            Action::CancelSignIn => {
                self.backend.send(Command::CancelSignIn);
                self.sign_in_url = None;
                self.auth = AuthStatus::SignedOut;
            }
            Action::SignOut => {
                self.backend.send(Command::SignOut);
                self.history = vec![Page::Home];
                self.history_index = 0;
            }
            Action::ToggleSidebar => {
                self.settings.sidebar_visible = !self.settings.sidebar_visible;
                self.settings_dirty = true;
            }
            Action::ToggleQueuePanel => {
                self.show_queue_panel = !self.show_queue_panel;
                if self.show_queue_panel {
                    self.show_lyrics_panel = false;
                    self.refresh_queue(true);
                }
            }
            Action::LyricsLineShown(line) => {
                // Escape or navigation may already have left the view earlier
                // in this frame. The returning panel still needs to reposition.
                if self.lyrics_fullscreen.is_some() {
                    self.lyrics_line_shown = Some(line);
                }
            }
            Action::FollowLyrics => {
                self.lyrics_following = true;
                self.lyrics_line_shown = None;
            }
            Action::PauseLyricsFollow => self.lyrics_following = false,
            Action::RetryLyrics => self.request_lyrics(),
            Action::ToggleDevicesPopup => {
                self.show_devices = !self.show_devices;
                if self.show_devices {
                    self.refresh_devices();
                    // Receivers waiting on the network are invisible to the
                    // Web API, so look for them ourselves.
                    self.backend.send(Command::DiscoverReceivers);
                }
            }
            Action::SetLibraryByYou(by_you) => {
                self.settings.library_by_you = by_you;
                self.mark_settings_dirty();
            }
            Action::SetLibrarySort { shelf, sort } => {
                if sort.supports(shelf) {
                    self.settings.library_sort.insert(shelf, sort);
                    match shelf {
                        crate::settings::LibraryShelf::Albums => self.library.albums.error = None,
                        crate::settings::LibraryShelf::Artists => self.library.artists.error = None,
                        crate::settings::LibraryShelf::Podcasts => self.library.shows.error = None,
                        crate::settings::LibraryShelf::Playlists => {}
                    }
                    self.mark_settings_dirty();
                }
            }
            Action::SetLibraryGrid(grid) => {
                self.settings.sidebar_grid = grid;
                self.mark_settings_dirty();
            }
            Action::ToggleLibraryFolder(id) => {
                if self.collapsed_folders.contains(&id) {
                    self.collapsed_folders.retain(|held| held != &id);
                } else {
                    self.collapsed_folders.push(id);
                }
                self.session_dirty = true;
            }
            Action::OpenThemesFolder => {
                self.backend.send(Command::OpenThemesFolder);
            }
            Action::EnablePlayback => {
                let free = self.user
                    .as_ref()
                    .and_then(|user| user.product.as_deref())
                    .is_some_and(|product| product != "premium");
                if free {
                    self.toast_error(self.localize.t(self.locale, "Local playback needs Spotify Premium"));
                } else if !self.local_ready
                    && !matches!(
                        self.local_playback,
                        LocalPlayback::Authorizing
                            | LocalPlayback::Connecting
                            | LocalPlayback::WaitingForNetwork
                    )
                {
                    self.settings.playback_authorized = true;
                    self.settings_dirty = true;
                    // The backend tries a saved grant first; the browser
                    // toast follows `LocalPlayback::Authorizing` (#818).
                    self.backend.send(Command::AuthorizePlayback);
                }
            }
            Action::SetPrereleaseSaved { uri, saved } => {
                if self.local_ready {
                    for page in self.album_pages.values_mut() {
                        if let Some(data) = &mut page.prerelease
                            && data.uri == uri
                            && data.saved.is_some()
                            && !data.saving
                        {
                            data.saved = Some(saved);
                            data.saving = true;
                            data.save_error = None;
                            self.backend.send(Command::SetPrereleaseSaved {
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
                self.plays.clear();
                if let Some(bytes) = self.plays.take_serialized() { self.files.write_private(&self.dirs.history_file(), bytes); }
                self.rebuild_recents();
                self.toast(self.localize.t(self.locale, "Play history cleared"));
            }
            Action::SetSkin(name) => {
                self.settings.skin = name;
                self.settings.random_skin = false;
                self.settings_dirty = true;
            }
            Action::SetRandomSkin(random) => {
                self.settings.random_skin = random;
                self.settings_dirty = true;
            }
            Action::SetSkinScale(scale) => {
                self.settings.skin_scale = Some(scale);
                self.settings_dirty = true;
            }
            Action::ToggleWinampPlaylist => {
                self.settings.playlist_open = !self.settings.playlist_open;
                self.settings_dirty = true;
                if self.settings.playlist_open {
                    self.refresh_queue(false);
                }
            }
            Action::ToggleWinampEq => {
                self.settings.eq_open = !self.settings.eq_open;
                self.settings_dirty = true;
            }
            Action::ToggleWinampShade => {
                self.settings.winamp_shaded = !self.settings.winamp_shaded;
                self.settings_dirty = true;
            }
            Action::ToggleWinampPlaylistShade => {
                self.settings.playlist_shaded = !self.settings.playlist_shaded;
                self.settings_dirty = true;
            }
            Action::ToggleWinampEqShade => {
                self.settings.eq_shaded = !self.settings.eq_shaded;
                self.settings_dirty = true;
            }
            // The same request the window's own close button makes, so the
            // close-to-tray setting decides what follows.
            Action::CyclePlayerBarVis => {
                self.settings.player_bar_vis = self.settings.player_bar_vis.next();
                self.settings_dirty = true;
            }
            Action::SetMilkdropSeconds(seconds) => {
                self.settings.milkdrop_seconds = seconds.clamp(1, 3600);
                self.settings_dirty = true;
            }
            Action::SetMilkdropScale(scale) => {
                self.settings.milkdrop_scale = scale.clamp(1, 4);
                self.settings_dirty = true;
            }
            _ => {}
        }
    }
}

