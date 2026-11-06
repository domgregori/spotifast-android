//! Spotifast's portable core, exposed to Kotlin over UniFFI.

mod auth_redirect;
mod credential_store;
mod wake;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use spotifast_core::api::PlayRequest;
use spotifast_core::api::models::{PlaylistItem, pick_image};
use spotifast_core::backend::{
    ApiRequest, ApiResponse, AuthStatus, Backend, Command, Event, LocalPlayback, LyricsRequest,
    PlaylistCacheRows, RecentsFor,
};
use spotifast_core::remote_action::RemoteAction;
use spotifast_core::config;
use spotifast_core::http::Http;
use spotifast_core::lyrics;
use spotifast_core::paths::AppDirs;
use spotifast_core::player::{LoadSpec, LocalState, Playback, PlayerCommand, RootlistEntry};
use spotifast_core::repeat::RepeatMode;
use spotifast_core::proxy::ProxyConfig;
use spotifast_core::services::{NoServices, Services};
use spotifast_core::settings::Settings;
use spotifast_core::vis::AudioTap;
use spotifast_core::wake::Wake;

use auth_redirect::AndroidAuthRedirect;
use credential_store::AndroidProtectedStore;
use wake::AndroidWake;

uniffi::setup_scaffolding!("spotifast");

/// Android loads this library via JNA (UniFFI's Kotlin bindings), not raw
/// JNI calls — but `System.loadLibrary` still invokes `JNI_OnLoad` for any
/// native library that exports it, JNA or not. cpal's AAudio backend (used
/// by `AndroidSink` in `spotifast_core::sink`) needs `ndk-context`'s global
/// `(JavaVM, Context)` populated before it can reach the system audio
/// manager; nothing else in the app ever provides it, so this is the one
/// chance to set it up. There is no Activity reference available this
/// early, so the Application context comes via the standard
/// `ActivityThread.currentApplication()` reflection trick.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: *mut jni::sys::JavaVM, _reserved: *mut std::ffi::c_void) -> jni::sys::jint {
    use jni::refs::Reference;

    // SAFETY: the Android runtime passes a valid JavaVM pointer to JNI_OnLoad.
    let vm = unsafe { jni::JavaVM::from_raw(vm) };
    let context: Result<usize, jni::errors::Error> = vm.attach_current_thread(|env| {
        let activity_thread = env.find_class(jni::jni_str!("android/app/ActivityThread"))?;
        let application = env
            .call_static_method(
                activity_thread,
                jni::jni_str!("currentApplication"),
                jni::jni_sig!("()Landroid/app/Application;"),
                &[],
            )?
            .l()?;
        let global = env.new_global_ref(application)?;
        let context_ptr = global.as_raw() as usize;
        // Leaked deliberately: ndk-context needs this valid for the process
        // lifetime, and the process owns exactly one Application.
        std::mem::forget(global);
        Ok(context_ptr)
    });
    match context {
        Ok(context_ptr) => {
            let vm_ptr = vm.get_raw().cast::<std::ffi::c_void>();
            unsafe {
                ndk_context::initialize_android_context(vm_ptr, context_ptr as *mut std::ffi::c_void);
            }
        }
        Err(error) => log::error!("JNI_OnLoad: couldn't initialize the ndk-context: {error}"),
    }
    jni::sys::JNI_VERSION_1_6
}

/// Outbound state changes, implemented by Kotlin and pushed into a flow that
/// Compose collects.
#[uniffi::export(foreign)]
pub trait PlaybackStateListener: Send + Sync {
    /// The sign-in state: one of `Starting`, `SignedOut`, `WaitingForBrowser`,
    /// `Connecting`, `Connected`, or `Failed`. `detail` carries the browser
    /// URL, the username, or the failure message as appropriate.
    fn on_auth_status(&self, status: String, detail: String);

    /// A user-facing error, shown as a toast.
    fn on_error(&self, message: String);

    /// The account's playlists, as a JSON array of `{ "id", "name" }`.
    fn on_library(&self, playlists: String);

    /// One playlist's tracks loaded so far, as a JSON array of
    /// `{ "uri", "name", "artist", "duration_ms" }` — the complete list
    /// loaded up to now, not just the page that just arrived. `has_more`
    /// is true while a further `load_more_playlist_tracks`/
    /// `load_all_playlist_tracks` (or the Liked Songs equivalents) call
    /// would fetch more.
    fn on_playlist_tracks(&self, playlist_id: String, tracks: String, has_more: bool);

    /// Search results for a query: the matching tracks, as a JSON array of
    /// `{ "uri", "name", "artist", "duration_ms" }`.
    fn on_search_tracks(&self, query: String, tracks: String);

    /// Local playback state: one of `Unavailable`, `Authorizing`, `Connecting`,
    /// `WaitingForNetwork`, `Ready`, or `Failed`.
    fn on_playback_status(&self, status: String, detail: String);

    /// The engine's current track/transport state, as JSON: `{ "playback",
    /// "loading", "track": { "uri", "title", "artist", "album", "art_url",
    /// "duration_ms" } | null, "position_ms", "shuffle", "repeat", "error" }`.
    /// Fires on every change librespot reports — track switches, play/pause,
    /// seeks, and load failures (`error` carries the message; the previous
    /// `track` stays as-is, matching librespot's own "still playing the old
    /// one until the new one is ready" behavior).
    fn on_now_playing(&self, state: String);

    /// Whether `uri` is in Liked Songs, confirmed after a `set_track_saved`
    /// or `check_track_saved` call.
    fn on_track_saved(&self, uri: String, saved: bool);

    /// Artists matching a search, as a JSON array of `{ "id", "name",
    /// "art_url" }`.
    fn on_search_artists(&self, query: String, artists: String);

    /// Albums matching a search, as a JSON array of `{ "id", "name",
    /// "artist", "art_url" }`.
    fn on_search_albums(&self, query: String, albums: String);

    /// Playlists matching a search, as a JSON array of `{ "id", "name",
    /// "art_url" }`.
    fn on_search_playlists(&self, query: String, playlists: String);

    /// The account's top tracks, as a JSON array of `{ "uri", "name",
    /// "artist", "duration_ms", "art_url" }`.
    fn on_top_tracks(&self, tracks: String);

    /// The account's top artists, as a JSON array of `{ "id", "name",
    /// "art_url" }`.
    fn on_top_artists(&self, artists: String);

    /// Available Spotify Connect devices, as a JSON array of `{ "id",
    /// "name", "kind", "is_active" }`.
    fn on_devices(&self, devices: String);

    /// Whether a `transfer_playback` call succeeded.
    fn on_device_transferred(&self, device_id: String, ok: bool);

    /// The upcoming tracks in Spotify's queue, as a JSON array of `{ "uri",
    /// "name", "artist", "duration_ms", "art_url" }`.
    fn on_queue(&self, tracks: String);

    /// Whether an `add_to_queue` call succeeded; `label` is the track name
    /// passed in, for a confirmation toast.
    fn on_queue_added(&self, label: String, ok: bool);

    /// The account's playlist tree in Spotify's own order, folders and all,
    /// as a JSON array of rows: `{ "kind": "playlist", "id" }`,
    /// `{ "kind": "folder_start", "id", "name" }`, or
    /// `{ "kind": "folder_end" }` — a folder's contents are every row
    /// between its `folder_start` and the matching `folder_end`. Row `id`s
    /// are bare ids, not full `spotify:...` URIs.
    fn on_rootlist(&self, rows: String);

    /// A playlist's name/art, resolved individually — for a rootlist row
    /// `library` has no entry for, typically one of Spotify's own
    /// algorithmic playlists (ids starting `37i9dQZF1`: Discover Weekly,
    /// a Daily Mix, Release Radar), which `/me/playlists` never returns
    /// even though the account has it pinned in its sidebar.
    fn on_playlist_info(&self, id: String, name: String, art_url: Option<String>);

    /// `playlist_info` found nothing — Spotify's Web API returns a plain
    /// 404 for its own algorithmic playlists (ids starting `37i9dQZF1`:
    /// Discover Weekly, a Daily Mix, Release Radar) no matter which
    /// account asks, so this is permanent, not worth retrying.
    fn on_playlist_unavailable(&self, id: String);

    /// The account's recently-played contexts, newest first, as a JSON
    /// array of `{ "uri", "played_at" }` — `uri` is a full `spotify:...`
    /// URI (a playlist, or the account's own Liked Songs collection), one
    /// entry per context, deduplicated to its most recent play.
    fn on_recently_played(&self, contexts: String);

    /// An artist's name/image, resolved via `artist_info`.
    fn on_artist_info(&self, id: String, name: String, art_url: Option<String>);

    /// The rest of an artist's profile — the same `artist_info` fetch, the
    /// fields `on_artist_info` doesn't carry. JSON: `{ "id", "followers",
    /// "genres": [...], "popularity": 0-100|null, "images": [url, ...] }`,
    /// images widest first.
    fn on_artist_details(&self, id: String, details: String);

    /// An artist's most popular tracks, as a JSON array of `{ "uri",
    /// "name", "artist", "duration_ms", "art_url" }`.
    fn on_artist_top_tracks(&self, id: String, tracks: String);

    /// An artist's albums and singles, as a JSON array of `{ "id", "name",
    /// "artist", "art_url" }`.
    fn on_artist_albums(&self, id: String, albums: String);

    /// An album's name/artist/image, resolved via `album_info`.
    fn on_album_info(&self, id: String, name: String, artist: String, art_url: Option<String>);

    /// `uri`'s album id, resolved via `track_album` — `album_id` is `None`
    /// if the track genuinely has none (vanishingly rare) or the lookup
    /// failed; either way, the now-playing screen just leaves "View
    /// album" out of its menu.
    fn on_track_album(&self, uri: String, album_id: Option<String>);

    /// One album's tracks, as a JSON array of `{ "uri", "name", "artist",
    /// "duration_ms", "art_url" }` — `art_url` is always `null`: an
    /// album's own tracks carry no cover of their own, distinct from the
    /// album's (shown once, in the header).
    fn on_album_tracks(&self, id: String, tracks: String);

    /// The account's saved albums, as a JSON array of `{ "id", "name",
    /// "artist", "art_url" }`, resolved via `saved_albums`.
    fn on_saved_albums(&self, albums: String);

    /// The account's followed artists, as a JSON array of `{ "id", "name",
    /// "art_url" }`, resolved via `followed_artists`.
    fn on_followed_artists(&self, artists: String);

    /// This week's new releases, as a JSON array of `{ "id", "name",
    /// "artist", "art_url" }`, resolved via `new_releases`.
    fn on_new_releases(&self, albums: String);

    /// The account's current playback, from `playback_state` — whichever
    /// Connect device is active. JSON: `{ "device_id", "is_playing",
    /// "position_ms", "shuffle", "repeat": "Off"|"Context"|"Track",
    /// "track": { "uri", "name", "artist", "duration_ms", "art_url",
    /// "artist_id", "album_id" } | null }`. `device_id` is `null` when
    /// nothing is playing anywhere on the account.
    fn on_remote_playback_state(&self, state: String);

    /// Lyrics for a track requested via `CoreHandle::lyrics`, as JSON:
    /// `{ "uri", "found", "synced", "instrumental", "lines": [{ "at_ms",
    /// "text" }], "error" }`. `found` is `false` when nobody has
    /// transcribed the track (not a fault); `error` is set only when the
    /// lookup itself failed (network, decode).
    fn on_lyrics(&self, lyrics: String);

    /// A `CoreHandle::radio` answer, as JSON: `{ "seed", "tracks": [{ "uri",
    /// "name", "artist", "duration_ms", "art_url" }], "error" }` —
    /// `tracks` is empty and `error` set when the lookup failed.
    fn on_radio(&self, radio: String);

    /// A `CoreHandle::create_playlist` call's outcome. `name` echoes the
    /// name it was called with, so Kotlin can tell which in-flight
    /// creation this answers (only one is ever tracked at a time —
    /// `on_error` alone can't disambiguate concurrent failures the way
    /// this dedicated callback can). `id` is the new playlist's bare id
    /// on success, empty on failure. `added`/`requested` report how many
    /// of the requested track uris actually made it in — `requested`
    /// is always whatever `create_playlist` was called with; `added` can
    /// fall short of it if a later chunk failed partway through.
    fn on_playlist_created(&self, name: String, id: String, added: u32, requested: u32, error: Option<String>);
}

/// Credential storage, implemented by Kotlin over EncryptedSharedPreferences.
#[uniffi::export(foreign)]
pub trait CredentialStore: Send + Sync {
    /// `None` when the key holds no credential.
    fn read(&self, key: String) -> Option<Vec<u8>>;

    /// Stores the credential under `key`; `false` when the store failed.
    fn write(&self, key: String, value: Vec<u8>) -> bool;

    fn delete(&self, key: String) -> bool;
}

/// A `create_playlist` call still waiting on `ApiResponse::PlaylistCreated`
/// or (for a non-empty track list) one of the follow-up
/// `ApiResponse::PlaylistItemsChanged` chunks. `ApiRequest::CreatePlaylist`/
/// `AddToPlaylist` carry no caller-supplied correlation id — and adding one
/// would change a shape the desktop app already pattern-matches against in
/// `src/app.rs` — so this crate tracks it itself instead, in a single slot
/// shared between whichever thread calls `create_playlist` and the event
/// pump that drives the chunked add-items sequence to completion.
struct PendingPlaylist {
    name: String,
    /// Remaining track uris still to add, taken 100 at a time (Spotify's
    /// own per-call limit on `/playlists/{id}/items`).
    remaining_uris: Vec<String>,
    requested: u32,
    added: u32,
    playlist_id: String,
    /// The size of the chunk most recently sent, so the next
    /// `PlaylistItemsChanged` knows how much to credit `added` with.
    last_chunk_len: u32,
}

/// Pagination state for one open playlist's tracks — shared between the
/// `playlist`/`load_more_playlist_tracks`/`load_all_playlist_tracks` FFI
/// calls (any thread Kotlin calls them from) and the event pump, which
/// fills it in as `PlaylistItems` pages arrive. Keyed by playlist id in
/// `CoreHandle::playlist_pages` — several playlist screens can be open
/// (backstack) at once, unlike Liked Songs below.
struct PlaylistPage {
    generation: u64,
    /// `None` once every track has been fetched.
    next_offset: Option<u32>,
    items: Vec<PlaylistItem>,
    /// Set by `load_all_playlist_tracks`: the pump requests the next page
    /// itself as each one lands, instead of waiting for another explicit
    /// `load_more_playlist_tracks` call.
    auto_continue: bool,
}

/// Same shape as `PlaylistPage`, for Liked Songs — always exactly one
/// list, so a bare `Option` rather than a map keyed by id.
struct LikedSongsPage {
    generation: u64,
    next_offset: Option<u32>,
    items: Vec<TrackEntry>,
    auto_continue: bool,
}

/// The long-lived core handle: owns the backend (and its tokio runtime) for
/// the process lifetime.
#[derive(uniffi::Object)]
pub struct CoreHandle {
    backend: Arc<Mutex<Backend>>,
    redirect: Arc<AndroidAuthRedirect>,
    listener: Mutex<Option<Arc<dyn PlaybackStateListener>>>,
    pump_started: AtomicBool,
    wake_rx: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    generation: AtomicU64,
    web_client_id: String,
    device_name: String,
    /// At most one `create_playlist` call tracked in flight at a time —
    /// see `PendingPlaylist`. Shared (not a plain event-pump-local
    /// accumulator like `playlists_fetch` etc.) because `create_playlist`
    /// itself, called from whichever thread Kotlin calls it on, needs to
    /// write the initial state before the event pump's thread can see it.
    pending_playlist: Arc<Mutex<Option<PendingPlaylist>>>,
    /// One entry per playlist ever opened via `playlist(id)` this session —
    /// shared for the same reason as `pending_playlist`: `load_more_playlist_tracks`/
    /// `load_all_playlist_tracks` need to read/advance `next_offset` from
    /// whichever thread Kotlin calls them on, not just the pump's own.
    playlist_pages: Arc<Mutex<HashMap<String, PlaylistPage>>>,
    liked_songs_page: Arc<Mutex<Option<LikedSongsPage>>>,
}

#[uniffi::export]
impl CoreHandle {
    #[uniffi::constructor]
    pub fn new(
        web_client_id: String,
        credential_store: Arc<dyn CredentialStore>,
        files_dir: String,
        bitrate_kbps: u16,
    ) -> Arc<Self> {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Debug)
                .with_tag("spotifast"),
        );
        let dirs = AppDirs::from_base(std::path::PathBuf::from(files_dir));
        let _ = dirs.ensure();
        // rustls-native-certs has no Android layout: point it at the system
        // trust store so librespot's HTTP client can verify Spotify.
        // SAFETY: called once at startup before any TLS handshake or other
        // thread reads the environment.
        unsafe {
            std::env::set_var("SSL_CERT_DIR", "/system/etc/security/cacerts");
        }
        log::debug!(
            "app dirs: config={:?} state={:?} cache={:?}",
            dirs.config,
            dirs.state,
            dirs.cache
        );
        let device_name = android_device_name(&dirs);
        // Baked into `engine_config` below, at session-construction time:
        // librespot has no live bitrate reconfiguration, so a change here
        // only takes effect the next time the app (and its session) starts.
        let settings = Settings {
            bitrate: bitrate_kbps,
            device_name: device_name.clone(),
            ..Settings::default()
        };
        let proxy = ProxyConfig::Off;
        let tap = AudioTap::new();
        let eq = spotifast_core::eq::shared();
        let engine_config = config::engine_config(&dirs, &settings, proxy.clone(), tap, eq);
        let http = Http::from_proxy(&proxy).unwrap_or_else(Http::unavailable);
        let redirect = Arc::new(AndroidAuthRedirect::default());
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        let waker: Arc<dyn Wake> = Arc::new(AndroidWake { signal: wake_tx });
        let services: Arc<dyn Services> = Arc::new(NoServices);
        let protected_store = AndroidProtectedStore::new(credential_store);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("spotifast-android")
            .enable_all()
            .build()
            .expect("unable to start the tokio runtime");

        let backend = Backend::spawn(
            dirs,
            engine_config,
            Some(web_client_id.clone()),
            waker,
            services,
            redirect.clone(),
            Some(Box::new(protected_store)),
            runtime,
            http,
            true,
        );

        Arc::new(Self {
            backend: Arc::new(Mutex::new(backend)),
            redirect,
            listener: Mutex::new(None),
            pump_started: AtomicBool::new(false),
            wake_rx: Mutex::new(Some(wake_rx)),
            generation: AtomicU64::new(0),
            web_client_id,
            device_name,
            pending_playlist: Arc::new(Mutex::new(None)),
            playlist_pages: Arc::new(Mutex::new(HashMap::new())),
            liked_songs_page: Arc::new(Mutex::new(None)),
        })
    }

    /// This install's own Spotify Connect device name — see
    /// [`android_device_name`]. Kotlin shows this on the mini player while
    /// this phone's local engine is the one that's "Ready" to play.
    pub fn device_name(&self) -> String {
        self.device_name.clone()
    }

    /// The core version, for smoke tests.
    pub fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// Start the Web API sign-in in the browser, using the configured client
    /// id (whose `spotifast://callback` redirect is registered). The browser
    /// URL arrives via `on_auth_status(WaitingForBrowser, url)`.
    pub fn sign_in(&self) {
        self.backend.lock().unwrap().send(Command::ConfigurePersonalWebApp(
            Some(self.web_client_id.clone()),
        ));
    }

    /// Kotlin calls this from `onNewIntent` with the captured `code`.
    pub fn complete_redirect(&self, code: String) {
        self.redirect.complete(code);
    }

    /// Cancel the in-flight browser sign-in.
    pub fn cancel_sign_in(&self) {
        self.backend.lock().unwrap().send(Command::CancelSignIn);
    }

    pub fn sign_out(&self) {
        self.backend.lock().unwrap().send(Command::SignOut);
    }

    /// Wipes every cached playlist page on disk — the "Clear Cache"
    /// Settings action's Rust half (the other half, clearing Coil's own
    /// image cache and the in-memory unavailable-playlist list, is Kotlin's
    /// to do, since neither one is reachable from here).
    pub fn clear_playlist_cache(&self) {
        self.backend.lock().unwrap().send(Command::ClearPlaylistCaches);
    }

    /// Wipes librespot's on-disk audio cache — the "Clear Cache" Settings
    /// action's other half, alongside `clear_playlist_cache`.
    pub fn clear_audio_cache(&self) {
        self.backend.lock().unwrap().send(Command::ClearAudioCache);
    }

    /// Request the account's playlist list; the result arrives via
    /// `on_library`.
    pub fn library(&self) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend.lock().unwrap().api(ApiRequest::MyPlaylists {
            offset: 0,
            generation,
        });
    }

    /// Request a playlist's first page of tracks; the result arrives via
    /// `on_playlist_tracks` — once immediately from disk if a cached copy
    /// exists (so opening a playlist again isn't a blank screen on a slow
    /// connection), then again with the live answer, which silently
    /// replaces it. `PLAYLIST_PAGE_SIZE` (50) tracks at a time — call
    /// `load_more_playlist_tracks`/`load_all_playlist_tracks` for the rest,
    /// guided by `on_playlist_tracks`'s `has_more` flag.
    pub fn playlist(&self, id: String) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        self.playlist_pages.lock().unwrap().insert(
            id.clone(),
            PlaylistPage { generation, next_offset: None, items: Vec::new(), auto_continue: false },
        );
        self.backend.lock().unwrap().send(Command::LoadPlaylistCache {
            id: id.clone(),
            generation,
        });
        self.backend.lock().unwrap().api(ApiRequest::PlaylistItems {
            id,
            offset: 0,
            generation,
        });
    }

    /// Fetches the next page of `id`'s tracks on top of what's already
    /// loaded — a no-op if everything is already loaded, or `id` was never
    /// opened via `playlist`. Meant for a scroll nearing the end of what's
    /// shown; the result arrives via `on_playlist_tracks`, same as the
    /// first page.
    pub fn load_more_playlist_tracks(&self, id: String) {
        let next = self
            .playlist_pages
            .lock()
            .unwrap()
            .get(&id)
            .and_then(|page| page.next_offset.map(|offset| (offset, page.generation)));
        if let Some((offset, generation)) = next {
            self.backend.lock().unwrap().api(ApiRequest::PlaylistItems { id, offset, generation });
        }
    }

    /// Like `load_more_playlist_tracks`, but keeps following every
    /// remaining page on its own instead of waiting for another call —
    /// the "Load all" button's Rust half.
    pub fn load_all_playlist_tracks(&self, id: String) {
        if let Some(page) = self.playlist_pages.lock().unwrap().get_mut(&id) {
            page.auto_continue = true;
        }
        self.load_more_playlist_tracks(id);
    }

    /// Search for a query; the matching tracks arrive via `on_search_tracks`.
    pub fn search(&self, query: String) {
        let serial = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend.lock().unwrap().api(ApiRequest::Search { query, serial });
    }

    /// Request Liked Songs' first page. The result arrives via
    /// `on_playlist_tracks` with the sentinel id `"__liked__"` — the same
    /// callback a real playlist's tracks use, so Kotlin treats it
    /// identically. `PLAYLIST_PAGE_SIZE` (50) at a time, same as a
    /// playlist — see `load_more_liked_songs`/`load_all_liked_songs`.
    pub fn liked_songs(&self) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        *self.liked_songs_page.lock().unwrap() =
            Some(LikedSongsPage { generation, next_offset: None, items: Vec::new(), auto_continue: false });
        self.backend.lock().unwrap().api(ApiRequest::SavedTracks {
            offset: 0,
            generation,
        });
    }

    /// Fetches the next page of Liked Songs on top of what's already
    /// loaded. See `load_more_playlist_tracks` — same shape, Liked Songs
    /// just has exactly one list rather than one per id.
    pub fn load_more_liked_songs(&self) {
        let next = self
            .liked_songs_page
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|page| page.next_offset.map(|offset| (offset, page.generation)));
        if let Some((offset, generation)) = next {
            self.backend.lock().unwrap().api(ApiRequest::SavedTracks { offset, generation });
        }
    }

    /// The "Load all" button's Rust half for Liked Songs.
    pub fn load_all_liked_songs(&self) {
        if let Some(page) = self.liked_songs_page.lock().unwrap().as_mut() {
            page.auto_continue = true;
        }
        self.load_more_liked_songs();
    }

    /// Saves or removes `uri` from Liked Songs. The confirmed result arrives
    /// via `on_track_saved`.
    pub fn set_track_saved(&self, uri: String, saved: bool) {
        self.backend
            .lock()
            .unwrap()
            .api(ApiRequest::SetSaved { uris: vec![uri], saved });
    }

    /// Asks whether `uri` is already saved; the answer arrives via
    /// `on_track_saved`.
    pub fn check_track_saved(&self, uri: String) {
        self.backend.lock().unwrap().api(ApiRequest::Contains { uris: vec![uri] });
    }

    /// Checks several tracks' liked state at once — up to 50 per Spotify's
    /// own limit on `/me/library/contains`; the caller is expected to
    /// chunk a longer list. Each result still arrives individually via
    /// `on_track_saved`, same as `check_track_saved`.
    pub fn check_tracks_saved(&self, uris: Vec<String>) {
        self.backend.lock().unwrap().api(ApiRequest::Contains { uris });
    }

    /// Creates a private playlist named `name`, then adds `track_uris` to
    /// it (chunked to Spotify's own 100-per-call limit on
    /// `/playlists/{id}/items`). The combined outcome — created, and
    /// however many of `track_uris` actually made it in — arrives via
    /// `on_playlist_created`. At most one call is tracked in flight at a
    /// time; call again only after the previous one's callback fired.
    pub fn create_playlist(&self, name: String, track_uris: Vec<String>) {
        *self.pending_playlist.lock().unwrap() = Some(PendingPlaylist {
            name: name.clone(),
            requested: track_uris.len() as u32,
            remaining_uris: track_uris,
            added: 0,
            playlist_id: String::new(),
            last_chunk_len: 0,
        });
        self.backend.lock().unwrap().api(ApiRequest::CreatePlaylist {
            name,
            public: false,
            description: "Made with Spotifast".to_string(),
        });
    }

    /// The account's playlist tree, folders and all, read from the
    /// session's own rootlist rather than the Web API (which has no public
    /// folder endpoint). The result arrives via `on_rootlist`.
    pub fn rootlist(&self) {
        self.backend.lock().unwrap().send(Command::Rootlist);
    }

    /// Resolves one playlist's name/art directly, for a rootlist row
    /// `library`'s `/me/playlists` sweep never returned. The result
    /// arrives via `on_playlist_info`.
    pub fn playlist_info(&self, id: String) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend.lock().unwrap().api(ApiRequest::Playlist { id, generation });
    }

    /// The account's top tracks; the result arrives via `on_top_tracks`.
    pub fn top_tracks(&self) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend.lock().unwrap().api(ApiRequest::TopTracks {
            offset: 0,
            full: true,
            generation,
        });
    }

    /// An artist's profile; the result arrives via `on_artist_info`.
    pub fn artist_info(&self, id: String) {
        self.backend.lock().unwrap().api(ApiRequest::Artist { id });
    }

    /// The account's saved albums, from the start; the result arrives via
    /// `on_saved_albums`.
    pub fn saved_albums(&self) {
        self.backend.lock().unwrap().api(ApiRequest::SavedAlbums { offset: 0 });
    }

    /// The account's followed artists, from the start; the result arrives
    /// via `on_followed_artists`.
    pub fn followed_artists(&self) {
        self.backend
            .lock()
            .unwrap()
            .api(ApiRequest::FollowedArtists { after: None });
    }

    /// This week's new releases, from the start; the result arrives via
    /// `on_new_releases`.
    pub fn new_releases(&self) {
        self.backend.lock().unwrap().api(ApiRequest::NewReleases { offset: 0 });
    }

    /// An artist's most popular tracks; the result arrives via
    /// `on_artist_top_tracks`.
    pub fn artist_top_tracks(&self, id: String) {
        self.backend
            .lock()
            .unwrap()
            .api(ApiRequest::ArtistTopTracks { id });
    }

    /// An artist's albums and singles, from the start; the result arrives
    /// via `on_artist_albums`. Compilations and "appears on" credits are
    /// left out — a discography view, not a full credits list.
    pub fn artist_albums(&self, id: String) {
        self.backend.lock().unwrap().api(ApiRequest::ArtistAlbums {
            id,
            groups: "album,single".to_string(),
            offset: 0,
        });
    }

    /// Resolves `uri`'s album id, for the now-playing screen's "View
    /// album" menu item — librespot's local playback metadata has no
    /// album id of its own (see `NowPlayingTrack`), so this is the one
    /// track lookup that exists purely to fill that gap. The result
    /// arrives via `on_track_album`.
    pub fn track_album(&self, uri: String) {
        let Some(id) = uri.strip_prefix("spotify:track:") else {
            return;
        };
        self.backend.lock().unwrap().api(ApiRequest::Track { id: id.to_string() });
    }

    /// An album's metadata; the result arrives via `on_album_info`.
    pub fn album_info(&self, id: String) {
        self.backend.lock().unwrap().api(ApiRequest::Album { id });
    }

    /// An album's tracks, from the start; a page beyond the first is
    /// requested internally as each page arrives, same as `playlist()`.
    /// The result arrives via `on_album_tracks`.
    pub fn album_tracks(&self, id: String) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend.lock().unwrap().api(ApiRequest::AlbumTracks {
            id,
            offset: 0,
            generation,
        });
    }

    /// The account's recently-played contexts (playlists and Liked Songs
    /// played from), newest first; the result arrives via
    /// `on_recently_played`. Backs the "Recently played" library sort.
    pub fn recently_played(&self) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend.lock().unwrap().api(ApiRequest::RecentlyPlayed {
            who: RecentsFor::Home,
            generation,
            before: None,
            limit: 50,
        });
    }

    /// The account's top artists; the result arrives via `on_top_artists`.
    pub fn top_artists(&self) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend
            .lock()
            .unwrap()
            .api(ApiRequest::TopArtists { generation });
    }

    /// Available Spotify Connect devices; the result arrives via
    /// `on_devices`.
    pub fn devices(&self) {
        self.backend.lock().unwrap().api(ApiRequest::Devices);
    }

    /// Transfers playback to `device_id`; the confirmation arrives via
    /// `on_device_transferred`.
    pub fn transfer_playback(&self, device_id: String) {
        self.backend
            .lock()
            .unwrap()
            .api(ApiRequest::Transfer { device_id, play: true });
    }

    /// Sets `device_id`'s volume (0..=100) through Spotify Connect — the
    /// only way to change a remote device's volume, since the phone's own
    /// volume buttons/slider only ever control local playback. No
    /// confirmation callback: the device's own next `devices()` poll
    /// reports the result, same as every other Connect device property.
    pub fn set_device_volume(&self, device_id: String, percent: u8) {
        self.backend.lock().unwrap().api(ApiRequest::Remote {
            action: RemoteAction::Volume,
            device_id: Some(device_id),
            play: None,
            position_ms: 0,
            percent,
            flag: false,
            repeat: String::new(),
        });
    }

    /// The upcoming tracks in Spotify's queue; the result arrives via
    /// `on_queue`.
    pub fn queue(&self) {
        let seq = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend.lock().unwrap().api(ApiRequest::Queue { seq });
    }

    /// Queues `uri` after whatever's already queued, on the active device;
    /// the confirmation arrives via `on_queue_added`.
    pub fn add_to_queue(&self, uri: String, label: String) {
        self.backend.lock().unwrap().api(ApiRequest::AddToQueue {
            uri,
            device_id: None,
            label,
        });
    }

    /// Authorize local playback in the browser (the streaming grant).
    pub fn authorize_playback(&self) {
        self.backend.lock().unwrap().send(Command::AuthorizePlayback);
    }

    /// Looks up lyrics for the track at `uri` — Spotify's own transcription
    /// first, LRCLIB after — caching the answer on disk either way. Kotlin
    /// passes the track's own metadata since nothing on the Rust side
    /// tracks "what's currently playing" independently of it; the result
    /// arrives via `on_lyrics`.
    pub fn lyrics(&self, uri: String, artist: String, title: String, album: String, duration_ms: u32) {
        self.backend.lock().unwrap().send(Command::Lyrics(Box::new(LyricsRequest {
            uri,
            query: lyrics::Query {
                artist,
                title,
                album,
                duration_ms,
            },
        })));
    }

    /// Resolves Spotify's radio for `seed` (a `spotify:track:...`,
    /// `:album:...`, `:artist:...`, or `:playlist:...` uri) through the
    /// streaming session — not the Web API, so it isn't subject to that
    /// API's Development Mode restrictions. The result arrives via
    /// `on_radio`.
    pub fn radio(&self, seed: String) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend
            .lock()
            .unwrap()
            .send(Command::Radio { seed, generation });
    }

    /// Starts local playback of `uris`, beginning at `start_index`. Kotlin
    /// calls this when the user taps a track row; the result arrives
    /// shortly after via `on_now_playing`.
    pub fn play_tracks(&self, uris: Vec<String>, start_index: u32) {
        let spec = LoadSpec {
            uris,
            offset_index: Some(start_index),
            play: true,
            ..LoadSpec::default()
        };
        self.backend
            .lock()
            .unwrap()
            .send(Command::Player(PlayerCommand::Load(spec)));
    }

    pub fn toggle_playback(&self) {
        self.backend.lock().unwrap().send(Command::Player(PlayerCommand::Toggle));
    }

    /// Jumps local playback to `position_ms` within the current track.
    pub fn seek(&self, position_ms: u32) {
        self.backend
            .lock()
            .unwrap()
            .send(Command::Player(PlayerCommand::Seek(position_ms)));
    }

    /// The account's current playback — whichever Connect device is
    /// active, not just this one. The result arrives via
    /// `on_remote_playback_state`; this is how the app notices (and keeps
    /// displaying) playback that moved to, or started on, a device other
    /// than this phone, since that device's own session never reports
    /// anything to this one directly.
    pub fn playback_state(&self) {
        let seq = self.generation.fetch_add(1, Ordering::Relaxed);
        self.backend
            .lock()
            .unwrap()
            .api(ApiRequest::PlaybackState { seq });
    }

    /// Resumes playback on `device_id` — a remote Connect device, not this
    /// one (local playback uses `toggle_playback`/the engine directly).
    pub fn remote_play(&self, device_id: String) {
        self.backend.lock().unwrap().api(ApiRequest::Remote {
            action: RemoteAction::Play,
            device_id: Some(device_id),
            play: None,
            position_ms: 0,
            percent: 0,
            flag: false,
            repeat: String::new(),
        });
    }

    /// Starts playing `uris` from `start_index` on `device_id` — the
    /// remote-device equivalent of `play_tracks`. Tapping a track while a
    /// remote device is active must go through this, not the local
    /// engine: loading locally calls `spirc.activate()`, which forcibly
    /// reclaims Connect control and stops whatever the remote device was
    /// playing, instead of telling that device to play the new list.
    pub fn remote_play_tracks(&self, device_id: String, uris: Vec<String>, start_index: u32) {
        let play = PlayRequest::tracks(uris).starting_at_index(start_index);
        self.backend.lock().unwrap().api(ApiRequest::Remote {
            action: RemoteAction::Play,
            device_id: Some(device_id),
            play: Some(play),
            position_ms: 0,
            percent: 0,
            flag: false,
            repeat: String::new(),
        });
    }

    pub fn remote_pause(&self, device_id: String) {
        self.backend.lock().unwrap().api(ApiRequest::Remote {
            action: RemoteAction::Pause,
            device_id: Some(device_id),
            play: None,
            position_ms: 0,
            percent: 0,
            flag: false,
            repeat: String::new(),
        });
    }

    pub fn remote_next(&self, device_id: String) {
        self.backend.lock().unwrap().api(ApiRequest::Remote {
            action: RemoteAction::Next,
            device_id: Some(device_id),
            play: None,
            position_ms: 0,
            percent: 0,
            flag: false,
            repeat: String::new(),
        });
    }

    pub fn remote_previous(&self, device_id: String) {
        self.backend.lock().unwrap().api(ApiRequest::Remote {
            action: RemoteAction::Previous,
            device_id: Some(device_id),
            play: None,
            position_ms: 0,
            percent: 0,
            flag: false,
            repeat: String::new(),
        });
    }

    pub fn remote_seek(&self, device_id: String, position_ms: u32) {
        self.backend.lock().unwrap().api(ApiRequest::Remote {
            action: RemoteAction::Seek,
            device_id: Some(device_id),
            play: None,
            position_ms,
            percent: 0,
            flag: false,
            repeat: String::new(),
        });
    }

    pub fn skip_next(&self) {
        self.backend.lock().unwrap().send(Command::Player(PlayerCommand::Next));
    }

    pub fn skip_previous(&self) {
        self.backend.lock().unwrap().send(Command::Player(PlayerCommand::Previous));
    }

    pub fn set_shuffle(&self, enabled: bool) {
        self.backend
            .lock()
            .unwrap()
            .send(Command::Player(PlayerCommand::Shuffle(enabled)));
    }

    /// `mode` is one of `"Off"`, `"Context"`, `"Track"`.
    pub fn set_repeat(&self, mode: String) {
        let mode = match mode.as_str() {
            "Context" => RepeatMode::Context,
            "Track" => RepeatMode::Track,
            _ => RepeatMode::Off,
        };
        self.backend
            .lock()
            .unwrap()
            .send(Command::Player(PlayerCommand::Repeat(mode)));
    }

    /// Register the outbound-state listener and start the event pump. Calling
    /// this again replaces the listener.
    pub fn set_listener(&self, listener: Arc<dyn PlaybackStateListener>) {
        *self.listener.lock().unwrap() = Some(listener.clone());

        if self.pump_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let backend = self.backend.clone();
        let pending_playlist = self.pending_playlist.clone();
        let playlist_pages = self.playlist_pages.clone();
        let liked_songs_page = self.liked_songs_page.clone();
        let wake_rx = self
            .wake_rx
            .lock()
            .unwrap()
            .take()
            .expect("the wake receiver is taken once");
        std::thread::Builder::new()
            .name("spotifast-android-events".to_string())
            .spawn(move || {
                // `MyPlaylists` pages 50 playlists at a time; an account
                // with more than one page's worth needs every page
                // followed before `on_library` reports a complete list —
                // otherwise a playlist past the first page is missing from
                // the flat library (though not from the rootlist, which
                // asks for up to 500 at once), and shows up in a folder
                // with its id standing in for a name nothing resolved.
                let mut playlists_fetch: Option<(u64, Vec<LibraryEntry>)> = None;
                // Same accumulator shape as `playlists_fetch`, for an
                // album's tracks past the first 50 (compilations and
                // deluxe editions routinely clear that).
                let mut album_tracks_fetch: Option<(u64, String, Vec<TrackEntry>)> = None;
                // `ArtistAlbums`'s response carries no generation counter —
                // correlated by (artist id, groups) instead, which is
                // unique enough since only one artist screen is ever open
                // at a time.
                let mut artist_albums_fetch: Option<(String, String, Vec<AlbumEntry>)> = None;
                loop {
                    // Drain and forward every queued event, then wait for the
                    // backend to signal the next change.
                    let events = backend.lock().unwrap().poll();
                    for event in events {
                        match event {
                            Event::Auth(status) => {
                                let (status, detail) = auth_status_parts(status);
                                listener.on_auth_status(status, detail);
                            }
                            Event::Error(message) => listener.on_error(message),
                            Event::Playback(status) => {
                                let (status, detail) = playback_status_parts(status);
                                listener.on_playback_status(status, detail);
                            }
                            Event::Local(state) => {
                                if let Ok(json) = serde_json::to_string(&NowPlayingState::from(&*state)) {
                                    listener.on_now_playing(json);
                                }
                            }
                            Event::Rootlist { result } => {
                                let rows: Vec<RootlistRow> = match result {
                                    Ok(rootlist) => rootlist
                                        .entries
                                        .into_iter()
                                        .filter_map(|entry| match entry {
                                            RootlistEntry::Playlist(uri) => {
                                                let id = uri.strip_prefix("spotify:playlist:")?.to_string();
                                                Some(RootlistRow::Playlist { id })
                                            }
                                            RootlistEntry::FolderStart { id, name } => {
                                                Some(RootlistRow::FolderStart { id, name })
                                            }
                                            RootlistEntry::FolderEnd => Some(RootlistRow::FolderEnd),
                                        })
                                        .collect(),
                                    Err(_) => Vec::new(),
                                };
                                if let Ok(json) = serde_json::to_string(&rows) {
                                    listener.on_rootlist(json);
                                }
                            }
                            // The on-disk half of `playlist()`'s read-through cache: no
                            // freshness check against the live answer (unlike desktop,
                            // which reconciles by snapshot id) — it's shown optimistically
                            // and the live `PlaylistItems` response arriving moments later
                            // overwrites it unconditionally either way.
                            Event::PlaylistCache { id, cache: Some(cache), .. } => {
                                let entries: Vec<TrackEntry> = cache
                                    .items
                                    .iter()
                                    .filter_map(|item| {
                                        let playable = item.playable()?;
                                        Some(TrackEntry {
                                            uri: playable.uri().to_string(),
                                            name: playable.name().to_string(),
                                            artist: playable.subtitle(),
                                            duration_ms: playable.duration_ms(),
                                            art_url: playable.image(TRACK_ART_TARGET).map(str::to_string),
                                            added_at: item.added_at.clone(),
                                            artist_id: playable.primary_artist_id().map(str::to_string),
                                            album_id: playable.album_id().map(str::to_string),
                                        })
                                    })
                                    .collect();
                                if let Ok(json) = serde_json::to_string(&entries) {
                                    // The cache is only ever written once a list is fully
                                    // loaded (see `StorePlaylistCache` below) — `has_more` is
                                    // always false here, correctly.
                                    listener.on_playlist_tracks(id, json, false);
                                }
                            }
                            Event::PlaylistCache { cache: None, .. } => {}
                            Event::Api(api) => match *api {
                                ApiResponse::MyPlaylists { generation, result, .. } => match result {
                                    Ok(page) => {
                                        let next_offset = page.next_offset();
                                        let mut entries: Vec<LibraryEntry> = page
                                            .items
                                            .into_iter()
                                            .map(|playlist| {
                                                let art_url = pick_image(&playlist.images, PLAYLIST_ART_TARGET)
                                                    .map(str::to_string);
                                                LibraryEntry {
                                                    id: playlist.id,
                                                    name: playlist.name,
                                                    art_url,
                                                }
                                            })
                                            .collect();
                                        if let Some((fetch_generation, existing)) = playlists_fetch.take() {
                                            if fetch_generation == generation {
                                                entries = existing.into_iter().chain(entries).collect();
                                            }
                                            // else: a newer fetch superseded this one; drop its partial items.
                                        }
                                        match next_offset {
                                            Some(next) => {
                                                playlists_fetch = Some((generation, entries));
                                                backend.lock().unwrap().api(ApiRequest::MyPlaylists {
                                                    offset: next,
                                                    generation,
                                                });
                                            }
                                            None => {
                                                if let Ok(json) = serde_json::to_string(&entries) {
                                                    listener.on_library(json);
                                                }
                                            }
                                        }
                                    }
                                    Err(_) => playlists_fetch = None,
                                },
                                ApiResponse::PlaylistItems { id, generation, result, .. } => {
                                    fn to_entries(items: &[PlaylistItem]) -> Vec<TrackEntry> {
                                        items
                                            .iter()
                                            .filter_map(|item| {
                                                let playable = item.playable()?;
                                                Some(TrackEntry {
                                                    uri: playable.uri().to_string(),
                                                    name: playable.name().to_string(),
                                                    artist: playable.subtitle(),
                                                    duration_ms: playable.duration_ms(),
                                                    art_url: playable.image(TRACK_ART_TARGET).map(str::to_string),
                                                    added_at: item.added_at.clone(),
                                                    artist_id: playable.primary_artist_id().map(str::to_string),
                                                    album_id: playable.album_id().map(str::to_string),
                                                })
                                            })
                                            .collect()
                                    }
                                    let mut pages = playlist_pages.lock().unwrap();
                                    // Stale: a later `playlist(id)` reopened this screen (new
                                    // generation) since this page was requested, or the screen
                                    // closed entirely (the entry is gone) — drop the answer.
                                    let Some(page) = pages.get_mut(&id).filter(|p| p.generation == generation) else {
                                        continue;
                                    };
                                    // Store-cache info only set when this page finishes the
                                    // list (`Ok` and no more remain) — written once below, after
                                    // the lock is released.
                                    let mut store_cache: Option<(u32, Vec<PlaylistItem>)> = None;
                                    if let Ok(response) = result {
                                        let total = response.total;
                                        page.next_offset = response.next_offset();
                                        page.items.extend(response.items);
                                        if page.next_offset.is_none() {
                                            store_cache = Some((total, page.items.clone()));
                                        }
                                    }
                                    // On `Err`, `next_offset`/`items` are left exactly as they
                                    // were — a prior page's own success still stands, and "Load
                                    // more"/"Load all" can simply be tried again.
                                    let has_more = page.next_offset.is_some();
                                    let next_offset = page.next_offset;
                                    let auto_continue = page.auto_continue;
                                    let entries_json = serde_json::to_string(&to_entries(&page.items));
                                    drop(pages);

                                    if let Some((total, items)) = store_cache {
                                        // Write the complete live answer through to disk for
                                        // next time's cache read. No real snapshot id is tracked
                                        // on the Android side (see the `PlaylistCache` read arm
                                        // above) — the live fetch always wins on arrival
                                        // regardless, so a placeholder is harmless.
                                        backend.lock().unwrap().send(Command::StorePlaylistCache {
                                            id: id.clone(),
                                            generation,
                                            snapshot: "android".to_string(),
                                            total,
                                            next_offset: None,
                                            rows: PlaylistCacheRows::Replace(items),
                                        });
                                    }
                                    if let Ok(json) = entries_json {
                                        listener.on_playlist_tracks(id.clone(), json, has_more);
                                    }
                                    // `PLAYLIST_PAGE_SIZE` (50) tracks at a time — "Load all"
                                    // keeps following every page itself instead of waiting for
                                    // another `load_more_playlist_tracks` call.
                                    if auto_continue {
                                        if let Some(next) = next_offset {
                                            backend.lock().unwrap().api(ApiRequest::PlaylistItems {
                                                id,
                                                offset: next,
                                                generation,
                                            });
                                        }
                                    }
                                }
                                ApiResponse::Search { query, result, .. } => {
                                    let (tracks, artists, albums) = match result {
                                        Ok(results) => {
                                            let tracks: Vec<TrackEntry> = results
                                                .tracks
                                                .map(|page| {
                                                    page.items
                                                        .into_iter()
                                                        .map(|track| {
                                                            let artist = track.artist_names();
                                                            let art_url = track
                                                                .image(TRACK_ART_TARGET)
                                                                .map(str::to_string);
                                                            let artist_id = track
                                                                .artists
                                                                .first()
                                                                .and_then(|a| a.id.clone());
                                                            let album_id = track
                                                                .album
                                                                .as_ref()
                                                                .map(|a| a.id.clone())
                                                                .filter(|id| !id.is_empty());
                                                            TrackEntry {
                                                                uri: track.uri,
                                                                name: track.name,
                                                                artist,
                                                                duration_ms: track.duration_ms,
                                                                art_url,
                                                                added_at: None,
                                                                artist_id,
                                                                album_id,
                                                            }
                                                        })
                                                        .collect()
                                                })
                                                .unwrap_or_default();
                                            let artists: Vec<ArtistEntry> = results
                                                .artists
                                                .map(|page| {
                                                    page.items
                                                        .into_iter()
                                                        .map(|artist| {
                                                            let art_url = pick_image(
                                                                &artist.images,
                                                                TRACK_ART_TARGET,
                                                            )
                                                            .map(str::to_string);
                                                            ArtistEntry {
                                                                id: artist.id,
                                                                name: artist.name,
                                                                art_url,
                                                            }
                                                        })
                                                        .collect()
                                                })
                                                .unwrap_or_default();
                                            let albums: Vec<AlbumEntry> = results
                                                .albums
                                                .map(|page| {
                                                    page.items
                                                        .into_iter()
                                                        .map(|album| {
                                                            let artist = album
                                                                .artists
                                                                .iter()
                                                                .map(|a| a.name.clone())
                                                                .collect::<Vec<_>>()
                                                                .join(", ");
                                                            let art_url = pick_image(
                                                                &album.images,
                                                                TRACK_ART_TARGET,
                                                            )
                                                            .map(str::to_string);
                                                            AlbumEntry {
                                                                id: album.id,
                                                                name: album.name,
                                                                artist,
                                                                art_url,
                                                            }
                                                        })
                                                        .collect()
                                                })
                                                .unwrap_or_default();
                                            (tracks, artists, albums)
                                        }
                                        Err(_) => (Vec::new(), Vec::new(), Vec::new()),
                                    };
                                    if let Ok(json) = serde_json::to_string(&tracks) {
                                        listener.on_search_tracks(query.clone(), json);
                                    }
                                    if let Ok(json) = serde_json::to_string(&artists) {
                                        listener.on_search_artists(query.clone(), json);
                                    }
                                    if let Ok(json) = serde_json::to_string(&albums) {
                                        listener.on_search_albums(query, json);
                                    }
                                    // Playlists are deliberately not read from this
                                    // response: once signed in (always, by the time
                                    // Search is reachable), the backend splits a query
                                    // into this catalogue-only request — which never
                                    // asks Spotify for the "playlist" type, so
                                    // `results.playlists` is always empty here — plus
                                    // a dedicated `ApiResponse::SearchPlaylists` below
                                    // that actually carries them. Extracting an
                                    // always-empty list here and forwarding it would
                                    // race that dedicated response and could blank out
                                    // real playlist results depending on arrival order.
                                }
                                ApiResponse::SearchPlaylists { query, result, .. } => {
                                    let playlists: Vec<LibraryEntry> = match result {
                                        Ok(page) => page
                                            .items
                                            .into_iter()
                                            .map(|playlist| {
                                                let art_url = pick_image(
                                                    &playlist.images,
                                                    PLAYLIST_ART_TARGET,
                                                )
                                                .map(str::to_string);
                                                LibraryEntry {
                                                    id: playlist.id,
                                                    name: playlist.name,
                                                    art_url,
                                                }
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&playlists) {
                                        listener.on_search_playlists(query, json);
                                    }
                                }
                                ApiResponse::TopTracks { result, .. } => {
                                    let entries: Vec<TrackEntry> = match result {
                                        Ok(page) => page
                                            .items
                                            .into_iter()
                                            .map(|track| {
                                                let artist = track.artist_names();
                                                let art_url =
                                                    track.image(TRACK_ART_TARGET).map(str::to_string);
                                                let artist_id = track.artists.first().and_then(|a| a.id.clone());
                                                let album_id = track
                                                    .album
                                                    .as_ref()
                                                    .map(|a| a.id.clone())
                                                    .filter(|id| !id.is_empty());
                                                TrackEntry {
                                                    uri: track.uri,
                                                    name: track.name,
                                                    artist,
                                                    duration_ms: track.duration_ms,
                                                    art_url,
                                                    added_at: None,
                                                    artist_id,
                                                    album_id,
                                                }
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_top_tracks(json);
                                    }
                                }
                                ApiResponse::TopArtists { result, .. } => {
                                    let entries: Vec<ArtistEntry> = match result {
                                        Ok(artists) => artists
                                            .into_iter()
                                            .map(|artist| {
                                                let art_url = pick_image(&artist.images, TRACK_ART_TARGET)
                                                    .map(str::to_string);
                                                ArtistEntry {
                                                    id: artist.id,
                                                    name: artist.name,
                                                    art_url,
                                                }
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_top_artists(json);
                                    }
                                }
                                ApiResponse::Devices(result) => {
                                    let entries: Vec<DeviceEntry> = match result {
                                        Ok(devices) => devices
                                            .into_iter()
                                            .map(|device| DeviceEntry {
                                                id: device.id.unwrap_or_default(),
                                                name: device.name,
                                                kind: device.kind,
                                                is_active: device.is_active,
                                                volume_percent: device.volume_percent,
                                                supports_volume: device.supports_volume.unwrap_or(false),
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_devices(json);
                                    }
                                }
                                ApiResponse::Transferred { device_id, result } => {
                                    listener.on_device_transferred(device_id, result.is_ok());
                                }
                                ApiResponse::Queue { result, .. } => {
                                    let entries: Vec<TrackEntry> = match result {
                                        Ok(queue) => queue
                                            .queue
                                            .into_iter()
                                            .map(|playable| TrackEntry {
                                                uri: playable.uri().to_string(),
                                                name: playable.name().to_string(),
                                                artist: playable.subtitle(),
                                                duration_ms: playable.duration_ms(),
                                                art_url: playable
                                                    .image(TRACK_ART_TARGET)
                                                    .map(str::to_string),
                                                added_at: None,
                                                artist_id: playable.primary_artist_id().map(str::to_string),
                                                album_id: playable.album_id().map(str::to_string),
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_queue(json);
                                    }
                                }
                                ApiResponse::QueueAdded { label, result } => {
                                    listener.on_queue_added(label, result.is_ok());
                                }
                                ApiResponse::Playlist { id, result, .. } => match result {
                                    Ok(playlist) => {
                                        let art_url =
                                            pick_image(&playlist.images, PLAYLIST_ART_TARGET).map(str::to_string);
                                        listener.on_playlist_info(id, playlist.name, art_url);
                                    }
                                    Err(_) => listener.on_playlist_unavailable(id),
                                },
                                ApiResponse::SavedTracks { generation, result, .. } => {
                                    let mut guard = liked_songs_page.lock().unwrap();
                                    // Stale: `liked_songs()` was called again (new generation)
                                    // since this page was requested — drop the answer.
                                    let Some(page) = guard.as_mut().filter(|p| p.generation == generation) else {
                                        continue;
                                    };
                                    if let Ok(response) = result {
                                        page.next_offset = response.next_offset();
                                        page.items.extend(response.items.into_iter().map(|saved| {
                                            let added_at = saved.added_at;
                                            let track = saved.track;
                                            let artist = track.artist_names();
                                            let art_url = track.image(TRACK_ART_TARGET).map(str::to_string);
                                            let artist_id = track.artists.first().and_then(|a| a.id.clone());
                                            let album_id = track
                                                .album
                                                .as_ref()
                                                .map(|a| a.id.clone())
                                                .filter(|id| !id.is_empty());
                                            TrackEntry {
                                                uri: track.uri,
                                                name: track.name,
                                                artist,
                                                duration_ms: track.duration_ms,
                                                art_url,
                                                added_at,
                                                artist_id,
                                                album_id,
                                            }
                                        }));
                                    }
                                    // On `Err`, `next_offset`/`items` are left exactly as they
                                    // were — a prior page's own success still stands, and "Load
                                    // more"/"Load all" can simply be tried again.
                                    let has_more = page.next_offset.is_some();
                                    let next_offset = page.next_offset;
                                    let auto_continue = page.auto_continue;
                                    let entries_json = serde_json::to_string(&page.items);
                                    drop(guard);

                                    if let Ok(json) = entries_json {
                                        listener.on_playlist_tracks("__liked__".to_string(), json, has_more);
                                    }
                                    // `PLAYLIST_PAGE_SIZE` (50) at a time, and Liked Songs is
                                    // exactly the library most likely to clear that — "Load all"
                                    // keeps following every page itself instead of waiting for
                                    // another `load_more_liked_songs` call.
                                    if auto_continue {
                                        if let Some(next) = next_offset {
                                            backend
                                                .lock()
                                                .unwrap()
                                                .api(ApiRequest::SavedTracks { offset: next, generation });
                                        }
                                    }
                                }
                                ApiResponse::SavedChanged { uris, saved, result } => {
                                    if result.is_ok() {
                                        if let Some(uri) = uris.into_iter().next() {
                                            listener.on_track_saved(uri, saved);
                                        }
                                    }
                                }
                                ApiResponse::Contains { uris, result } => {
                                    if let Ok(flags) = result {
                                        for (uri, saved) in uris.into_iter().zip(flags) {
                                            listener.on_track_saved(uri, saved);
                                        }
                                    }
                                }
                                ApiResponse::Artist { id, result } => match result {
                                    Ok(artist) => {
                                        let art_url =
                                            pick_image(&artist.images, PLAYLIST_ART_TARGET).map(str::to_string);
                                        let mut images: Vec<(u32, String)> = artist
                                            .images
                                            .iter()
                                            .map(|image| (image.width.unwrap_or(0), image.url.clone()))
                                            .collect();
                                        images.sort_by(|a, b| b.0.cmp(&a.0));
                                        let details = ArtistDetailsEntry {
                                            id: id.clone(),
                                            followers: artist.followers.map(|f| f.total).unwrap_or(0),
                                            genres: artist.genres.clone(),
                                            popularity: artist.popularity,
                                            images: images.into_iter().map(|(_, url)| url).collect(),
                                        };
                                        if let Ok(json) = serde_json::to_string(&details) {
                                            listener.on_artist_details(id.clone(), json);
                                        }
                                        listener.on_artist_info(id, artist.name, art_url);
                                    }
                                    Err(_) => listener.on_error("Couldn't load this artist".to_string()),
                                },
                                ApiResponse::ArtistTopTracks { id, result } => {
                                    let entries: Vec<TrackEntry> = match result {
                                        Ok(tracks) => tracks
                                            .into_iter()
                                            .map(|track| {
                                                let artist = track.artist_names();
                                                let art_url =
                                                    track.image(TRACK_ART_TARGET).map(str::to_string);
                                                let artist_id =
                                                    track.artists.first().and_then(|a| a.id.clone());
                                                let album_id = track
                                                    .album
                                                    .as_ref()
                                                    .map(|a| a.id.clone())
                                                    .filter(|id| !id.is_empty());
                                                TrackEntry {
                                                    uri: track.uri,
                                                    name: track.name,
                                                    artist,
                                                    duration_ms: track.duration_ms,
                                                    art_url,
                                                    added_at: None,
                                                    artist_id,
                                                    album_id,
                                                }
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_artist_top_tracks(id, json);
                                    }
                                }
                                ApiResponse::ArtistAlbums { id, groups, offset: _, result } => match result {
                                    Ok(page) => {
                                        let next_offset = page.next_offset();
                                        let mut entries: Vec<AlbumEntry> = page
                                            .items
                                            .into_iter()
                                            .map(|album| {
                                                let artist = album
                                                    .artists
                                                    .iter()
                                                    .map(|a| a.name.clone())
                                                    .collect::<Vec<_>>()
                                                    .join(", ");
                                                let art_url =
                                                    pick_image(&album.images, TRACK_ART_TARGET).map(str::to_string);
                                                AlbumEntry { id: album.id, name: album.name, artist, art_url }
                                            })
                                            .collect();
                                        if let Some((fetch_id, fetch_groups, existing)) =
                                            artist_albums_fetch.take()
                                        {
                                            if fetch_id == id && fetch_groups == groups {
                                                entries = existing.into_iter().chain(entries).collect();
                                            }
                                        }
                                        match next_offset {
                                            Some(next) => {
                                                artist_albums_fetch =
                                                    Some((id.clone(), groups.clone(), entries));
                                                backend.lock().unwrap().api(ApiRequest::ArtistAlbums {
                                                    id,
                                                    groups,
                                                    offset: next,
                                                });
                                            }
                                            None => {
                                                if let Ok(json) = serde_json::to_string(&entries) {
                                                    listener.on_artist_albums(id, json);
                                                }
                                            }
                                        }
                                    }
                                    Err(_) => artist_albums_fetch = None,
                                },
                                ApiResponse::SavedAlbums { result, .. } => {
                                    let entries: Vec<AlbumEntry> = match result {
                                        Ok(page) => page
                                            .items
                                            .into_iter()
                                            .map(|saved| {
                                                let album = saved.album;
                                                let artist = album
                                                    .artists
                                                    .iter()
                                                    .map(|a| a.name.clone())
                                                    .collect::<Vec<_>>()
                                                    .join(", ");
                                                let art_url = pick_image(&album.images, TRACK_ART_TARGET)
                                                    .map(str::to_string);
                                                AlbumEntry { id: album.id, name: album.name, artist, art_url }
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_saved_albums(json);
                                    }
                                }
                                ApiResponse::FollowedArtists { result, .. } => {
                                    let entries: Vec<ArtistEntry> = match result {
                                        Ok(page) => page
                                            .items
                                            .into_iter()
                                            .map(|artist| {
                                                let art_url = pick_image(&artist.images, TRACK_ART_TARGET)
                                                    .map(str::to_string);
                                                ArtistEntry { id: artist.id, name: artist.name, art_url }
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_followed_artists(json);
                                    }
                                }
                                ApiResponse::NewReleases { result, .. } => {
                                    let entries: Vec<AlbumEntry> = match result {
                                        Ok(page) => page
                                            .items
                                            .into_iter()
                                            .map(|album| {
                                                let artist = album
                                                    .artists
                                                    .iter()
                                                    .map(|a| a.name.clone())
                                                    .collect::<Vec<_>>()
                                                    .join(", ");
                                                let art_url = pick_image(&album.images, TRACK_ART_TARGET)
                                                    .map(str::to_string);
                                                AlbumEntry { id: album.id, name: album.name, artist, art_url }
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_new_releases(json);
                                    }
                                }
                                ApiResponse::Track { id, result } => {
                                    let album_id = result.ok().and_then(|track| {
                                        track
                                            .album
                                            .filter(|album| !album.id.is_empty())
                                            .map(|album| album.id)
                                    });
                                    listener.on_track_album(format!("spotify:track:{id}"), album_id);
                                }
                                ApiResponse::Album { id, result } => match result {
                                    Ok(album) => {
                                        let artist = album
                                            .artists
                                            .iter()
                                            .map(|a| a.name.clone())
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        let art_url =
                                            pick_image(&album.images, PLAYLIST_ART_TARGET).map(str::to_string);
                                        listener.on_album_info(id, album.name, artist, art_url);
                                    }
                                    Err(_) => listener.on_error("Couldn't load this album".to_string()),
                                },
                                ApiResponse::AlbumTracks { id, generation, result, .. } => match result {
                                    Ok(page) => {
                                        let next_offset = page.next_offset();
                                        let mut entries: Vec<TrackEntry> = page
                                            .items
                                            .into_iter()
                                            .map(|track| {
                                                let artist = track.artist_names();
                                                let artist_id =
                                                    track.artists.first().and_then(|a| a.id.clone());
                                                TrackEntry {
                                                    uri: track.uri,
                                                    name: track.name,
                                                    artist,
                                                    duration_ms: track.duration_ms,
                                                    art_url: None,
                                                    added_at: None,
                                                    artist_id,
                                                    // `/albums/{id}/tracks` omits the album on its own
                                                    // tracks — nothing to navigate back to.
                                                    album_id: None,
                                                }
                                            })
                                            .collect();
                                        if let Some((fetch_generation, fetch_id, existing)) =
                                            album_tracks_fetch.take()
                                        {
                                            if fetch_generation == generation && fetch_id == id {
                                                entries = existing.into_iter().chain(entries).collect();
                                            }
                                        }
                                        match next_offset {
                                            Some(next) => {
                                                album_tracks_fetch = Some((generation, id.clone(), entries));
                                                backend.lock().unwrap().api(ApiRequest::AlbumTracks {
                                                    id,
                                                    offset: next,
                                                    generation,
                                                });
                                            }
                                            None => {
                                                if let Ok(json) = serde_json::to_string(&entries) {
                                                    listener.on_album_tracks(id, json);
                                                }
                                            }
                                        }
                                    }
                                    Err(_) => album_tracks_fetch = None,
                                },
                                ApiResponse::PlaybackState { result, .. } => {
                                    let entry = match result {
                                        Ok(Some(state)) => {
                                            let device_id = state.device.as_ref().and_then(|d| d.id.clone());
                                            let track = state.item.as_ref().map(|playable| {
                                                let album = match playable {
                                                    spotifast_core::api::models::PlayableItem::Track(track) => {
                                                        track.album.as_ref().map_or_else(String::new, |album| {
                                                            album.name.clone()
                                                        })
                                                    }
                                                    spotifast_core::api::models::PlayableItem::Episode(episode) => {
                                                        episode.show.as_ref().map_or_else(String::new, |show| {
                                                            show.name.clone()
                                                        })
                                                    }
                                                };
                                                NowPlayingTrack {
                                                    uri: playable.uri().to_string(),
                                                    title: playable.name().to_string(),
                                                    artist: playable.subtitle(),
                                                    artist_id: playable.primary_artist_id().map(str::to_string),
                                                    album,
                                                    album_id: playable.album_id().map(str::to_string),
                                                    art_url: playable.image(TRACK_ART_TARGET).map(str::to_string),
                                                    duration_ms: playable.duration_ms(),
                                                }
                                            });
                                            let (context_kind, context_id) = state
                                                .context
                                                .as_ref()
                                                .filter(|context| {
                                                    matches!(context.kind.as_str(), "playlist" | "album" | "artist")
                                                })
                                                .map(|context| {
                                                    let id = context.uri.rsplit(':').next().unwrap_or_default();
                                                    (context.kind.clone(), id.to_string())
                                                })
                                                .unzip();
                                            RemotePlaybackEntry {
                                                device_id,
                                                is_playing: state.is_playing,
                                                position_ms: state.progress_ms.unwrap_or(0),
                                                shuffle: state.shuffle_state,
                                                repeat: match state.repeat_state.as_str() {
                                                    "context" => "Context",
                                                    "track" => "Track",
                                                    _ => "Off",
                                                }
                                                .to_string(),
                                                track,
                                                context_kind,
                                                context_id,
                                            }
                                        }
                                        Ok(None) | Err(_) => RemotePlaybackEntry {
                                            device_id: None,
                                            is_playing: false,
                                            position_ms: 0,
                                            shuffle: false,
                                            repeat: "Off".to_string(),
                                            track: None,
                                            context_kind: None,
                                            context_id: None,
                                        },
                                    };
                                    if let Ok(json) = serde_json::to_string(&entry) {
                                        listener.on_remote_playback_state(json);
                                    }
                                }
                                ApiResponse::RecentlyPlayed { result, .. } => {
                                    let mut seen = std::collections::HashSet::new();
                                    let entries: Vec<RecentContextEntry> = match result {
                                        Ok(page) => page
                                            .items
                                            .into_iter()
                                            .filter_map(|play| {
                                                let uri = play.context?.uri;
                                                let sidebar =
                                                    uri.contains(":playlist:") || uri.contains(":collection");
                                                if !sidebar || !seen.insert(uri.clone()) {
                                                    return None;
                                                }
                                                Some(RecentContextEntry { uri, played_at: play.played_at })
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_recently_played(json);
                                    }
                                }
                                ApiResponse::PlaylistCreated(result) => {
                                    let mut finished: Option<(String, String, u32, u32, Option<String>)> = None;
                                    let mut next_chunk: Option<(String, String, Vec<String>)> = None;
                                    match result {
                                        Ok(playlist) => {
                                            let mut state = pending_playlist
                                                .lock()
                                                .unwrap()
                                                .take()
                                                .unwrap_or_else(|| PendingPlaylist {
                                                    name: playlist.name.clone(),
                                                    remaining_uris: Vec::new(),
                                                    requested: 0,
                                                    added: 0,
                                                    playlist_id: String::new(),
                                                    last_chunk_len: 0,
                                                });
                                            state.playlist_id = playlist.id;
                                            if state.remaining_uris.is_empty() {
                                                finished = Some((state.name, state.playlist_id, 0, 0, None));
                                            } else {
                                                let chunk: Vec<String> = state
                                                    .remaining_uris
                                                    .drain(..state.remaining_uris.len().min(100))
                                                    .collect();
                                                state.last_chunk_len = chunk.len() as u32;
                                                let playlist_id = state.playlist_id.clone();
                                                let name = state.name.clone();
                                                *pending_playlist.lock().unwrap() = Some(state);
                                                next_chunk = Some((playlist_id, name, chunk));
                                            }
                                        }
                                        Err(error) => {
                                            let name = pending_playlist
                                                .lock()
                                                .unwrap()
                                                .take()
                                                .map(|p| p.name)
                                                .unwrap_or_default();
                                            finished = Some((name, String::new(), 0, 0, Some(error.to_string())));
                                        }
                                    }
                                    if let Some((name, id, added, requested, error)) = finished {
                                        listener.on_playlist_created(name, id, added, requested, error);
                                    }
                                    if let Some((playlist_id, playlist_name, uris)) = next_chunk {
                                        backend.lock().unwrap().api(ApiRequest::AddToPlaylist {
                                            playlist_id,
                                            playlist_name,
                                            uris,
                                            position: None,
                                        });
                                    }
                                }
                                ApiResponse::PlaylistItemsChanged { id, result, .. } => {
                                    let mut finished: Option<(String, String, u32, u32, Option<String>)> = None;
                                    let mut next_chunk: Option<(String, String, Vec<String>)> = None;
                                    {
                                        let mut guard = pending_playlist.lock().unwrap();
                                        if let Some(state) = guard.as_mut().filter(|p| p.playlist_id == id) {
                                            if result.is_ok() {
                                                state.added += state.last_chunk_len;
                                            }
                                            if result.is_err() || state.remaining_uris.is_empty() {
                                                let state = guard.take().unwrap();
                                                let error = result.err().map(|e| e.to_string());
                                                finished = Some((
                                                    state.name,
                                                    state.playlist_id,
                                                    state.added,
                                                    state.requested,
                                                    error,
                                                ));
                                            } else {
                                                let chunk: Vec<String> = state
                                                    .remaining_uris
                                                    .drain(..state.remaining_uris.len().min(100))
                                                    .collect();
                                                state.last_chunk_len = chunk.len() as u32;
                                                next_chunk = Some((
                                                    state.playlist_id.clone(),
                                                    state.name.clone(),
                                                    chunk,
                                                ));
                                            }
                                        }
                                    }
                                    if let Some((name, id, added, requested, error)) = finished {
                                        listener.on_playlist_created(name, id, added, requested, error);
                                    }
                                    if let Some((playlist_id, playlist_name, uris)) = next_chunk {
                                        backend.lock().unwrap().api(ApiRequest::AddToPlaylist {
                                            playlist_id,
                                            playlist_name,
                                            uris,
                                            position: None,
                                        });
                                    }
                                }
                                _ => {}
                            },
                            Event::Lyrics { uri, result } => {
                                let entry = match result {
                                    Ok(Some(found)) => LyricsEntry {
                                        uri,
                                        found: true,
                                        synced: found.synced,
                                        instrumental: found.instrumental,
                                        lines: found
                                            .lines
                                            .into_iter()
                                            .map(|line| LyricsLineEntry {
                                                at_ms: line.at_ms,
                                                text: line.text,
                                            })
                                            .collect(),
                                        error: None,
                                    },
                                    Ok(None) => LyricsEntry {
                                        uri,
                                        found: false,
                                        synced: false,
                                        instrumental: false,
                                        lines: Vec::new(),
                                        error: None,
                                    },
                                    Err(error) => LyricsEntry {
                                        uri,
                                        found: false,
                                        synced: false,
                                        instrumental: false,
                                        lines: Vec::new(),
                                        error: Some(error),
                                    },
                                };
                                if let Ok(json) = serde_json::to_string(&entry) {
                                    listener.on_lyrics(json);
                                }
                            }
                            Event::Radio { seed, result, .. } => {
                                let entry = match result {
                                    Ok(tracks) => RadioEntry {
                                        seed,
                                        tracks: tracks
                                            .into_iter()
                                            .map(|track| {
                                                let artist = track.artist_names();
                                                let art_url = track
                                                    .image(TRACK_ART_TARGET)
                                                    .map(str::to_string);
                                                TrackEntry {
                                                    uri: track.uri,
                                                    name: track.name,
                                                    artist,
                                                    duration_ms: track.duration_ms,
                                                    art_url,
                                                    added_at: None,
                                                    artist_id: track
                                                        .artists
                                                        .first()
                                                        .and_then(|a| a.id.clone()),
                                                    album_id: track
                                                        .album
                                                        .as_ref()
                                                        .map(|a| a.id.clone())
                                                        .filter(|id| !id.is_empty()),
                                                }
                                            })
                                            .collect(),
                                        error: None,
                                    },
                                    Err(error) => RadioEntry {
                                        seed,
                                        tracks: Vec::new(),
                                        error: Some(error),
                                    },
                                };
                                if let Ok(json) = serde_json::to_string(&entry) {
                                    listener.on_radio(json);
                                }
                            }
                            _ => {}
                        }
                    }
                    if wake_rx.recv().is_err() {
                        return;
                    }
                }
            })
            .expect("unable to start the event pump");
    }
}

/// This install's own Spotify Connect device name: `spotifast-<petname>`,
/// generated once (via the `petname` crate) and cached under `dirs.state`
/// so it's stable across restarts. Desktop's own session always registers
/// as plain "Spotifast"; without this, every Android install would too,
/// and a desktop instance and this one would be indistinguishable in
/// Spotify's own device picker.
fn android_device_name(dirs: &AppDirs) -> String {
    let path = dirs.state.join("device_name.txt");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let word = petname::petname(2, "-").unwrap_or_else(|| "device".to_string());
    let name = format!("spotifast-{word}");
    let _ = std::fs::write(&path, &name);
    name
}

/// Splits an [`AuthStatus`] into a tag and its optional detail for Kotlin.
fn auth_status_parts(status: AuthStatus) -> (String, String) {
    match status {
        AuthStatus::Starting => ("Starting".into(), String::new()),
        AuthStatus::SignedOut => ("SignedOut".into(), String::new()),
        AuthStatus::WaitingForBrowser { url } => ("WaitingForBrowser".into(), url),
        AuthStatus::Connecting => ("Connecting".into(), String::new()),
        AuthStatus::Connected { username } => ("Connected".into(), username),
        AuthStatus::Failed(message) => ("Failed".into(), message),
    }
}

/// Splits a [`LocalPlayback`] into a tag and its optional detail for Kotlin.
fn playback_status_parts(status: LocalPlayback) -> (String, String) {
    match status {
        LocalPlayback::Unavailable => ("Unavailable".into(), String::new()),
        LocalPlayback::Authorizing => ("Authorizing".into(), String::new()),
        LocalPlayback::Connecting => ("Connecting".into(), String::new()),
        LocalPlayback::WaitingForNetwork => ("WaitingForNetwork".into(), String::new()),
        LocalPlayback::Ready { device_id } => ("Ready".into(), device_id),
        LocalPlayback::Failed(message) => ("Failed".into(), message),
    }
}

/// The engine's track/transport state, serialized to Kotlin as JSON.
#[derive(serde::Serialize)]
struct NowPlayingState {
    /// One of `Stopped`, `Loading`, `Playing`, `Paused`.
    playback: &'static str,
    track: Option<NowPlayingTrack>,
    position_ms: u32,
    shuffle: bool,
    /// One of `Off`, `Context`, `Track`.
    repeat: &'static str,
    error: Option<String>,
}

#[derive(serde::Serialize)]
struct NowPlayingTrack {
    uri: String,
    title: String,
    artist: String,
    /// The primary artist's id, for a "View artist" link — the album has
    /// no equivalent here when this reports local playback: librespot's
    /// local playback metadata only carries the album's name, not its
    /// catalog id (see `on_track_album` for how the app resolves that
    /// separately, on demand). `GET /me/player` carries the real id
    /// directly, so remote playback (`RemotePlaybackEntry`) fills this in
    /// up front instead.
    artist_id: Option<String>,
    album: String,
    album_id: Option<String>,
    art_url: Option<String>,
    duration_ms: u32,
}

impl From<&LocalState> for NowPlayingState {
    fn from(state: &LocalState) -> Self {
        Self {
            playback: match state.playback {
                Playback::Stopped => "Stopped",
                Playback::Loading => "Loading",
                Playback::Playing => "Playing",
                Playback::Paused => "Paused",
            },
            track: state.track.as_ref().map(|track| NowPlayingTrack {
                uri: track.uri.clone(),
                title: track.title.clone(),
                artist: track.artist_names(),
                artist_id: track.artists.first().and_then(|artist| artist.id.clone()),
                album: track.album.clone(),
                album_id: None,
                art_url: track.art_url.clone(),
                duration_ms: track.duration_ms,
            }),
            position_ms: state.position_now(),
            shuffle: state.shuffle,
            repeat: match state.repeat {
                RepeatMode::Off => "Off",
                RepeatMode::Context => "Context",
                RepeatMode::Track => "Track",
            },
            error: state.error.clone(),
        }
    }
}

/// The pixel width asked for a playlist's cover (reused for both library
/// rows and the larger playlist-detail header).
const PLAYLIST_ART_TARGET: u32 = 300;
/// The pixel width asked for a track's cover in search/playlist rows.
const TRACK_ART_TARGET: u32 = 128;

/// One playlist in the bare library list, serialized to Kotlin as JSON.
#[derive(serde::Serialize)]
struct LibraryEntry {
    id: String,
    name: String,
    art_url: Option<String>,
}

/// One track in a playlist, serialized to Kotlin as JSON. `added_at` is
/// only ever populated for a real playlist/Liked Songs row (Spotify's own
/// per-track timestamp) — search, top tracks, and the queue leave it `null`
/// since there's no such timestamp for those.
#[derive(serde::Serialize)]
struct TrackEntry {
    uri: String,
    name: String,
    artist: String,
    duration_ms: u32,
    art_url: Option<String>,
    added_at: Option<String>,
    /// The primary artist's id, for a "View artist" menu item; `None` for
    /// an episode.
    artist_id: Option<String>,
    /// The track's album id, for a "View album" menu item; `None` for an
    /// episode, or a track fetched already scoped to one album (an
    /// album's own track list has no use navigating back to itself).
    album_id: Option<String>,
}

/// One recently-played context (a playlist, or Liked Songs), serialized to
/// Kotlin as JSON.
#[derive(serde::Serialize)]
struct RecentContextEntry {
    uri: String,
    played_at: Option<String>,
}

/// One lyrics line, serialized to Kotlin as JSON.
#[derive(serde::Serialize)]
struct LyricsLineEntry {
    at_ms: Option<u32>,
    text: String,
}

/// The rest of an artist's profile, serialized to Kotlin as JSON —
/// `on_artist_info`'s companion with the fields it doesn't carry.
#[derive(serde::Serialize)]
struct ArtistDetailsEntry {
    id: String,
    followers: u64,
    genres: Vec<String>,
    popularity: Option<u8>,
    images: Vec<String>,
}

/// A `CoreHandle::lyrics` answer, serialized to Kotlin as JSON. `found` is
/// `false` when nobody's transcribed the track (not a fault); `error` is
/// set only when the lookup itself failed.
#[derive(serde::Serialize)]
struct LyricsEntry {
    uri: String,
    found: bool,
    synced: bool,
    instrumental: bool,
    lines: Vec<LyricsLineEntry>,
    error: Option<String>,
}

/// A `CoreHandle::radio` answer, serialized to Kotlin as JSON.
#[derive(serde::Serialize)]
struct RadioEntry {
    seed: String,
    tracks: Vec<TrackEntry>,
    error: Option<String>,
}

/// The account's current playback, from `GET /me/player` — whichever
/// Connect device is active, serialized to Kotlin as JSON.
#[derive(serde::Serialize)]
struct RemotePlaybackEntry {
    device_id: Option<String>,
    is_playing: bool,
    position_ms: u32,
    shuffle: bool,
    /// One of `"Off"`, `"Context"`, `"Track"`.
    repeat: String,
    track: Option<NowPlayingTrack>,
    /// What's playing from, if anything — `"playlist"`, `"album"`, or
    /// `"artist"` (Spotify's own `context.type`; other values, like
    /// `"collection"` for Liked Songs, are left out since there's no
    /// screen to navigate to for them), paired with the id out of
    /// `context.uri`. The "go to source" button's data.
    context_kind: Option<String>,
    context_id: Option<String>,
}

/// One artist in a search or top-artists result, serialized to Kotlin as
/// JSON.
#[derive(serde::Serialize)]
struct ArtistEntry {
    id: String,
    name: String,
    art_url: Option<String>,
}

/// One album in a search result, serialized to Kotlin as JSON.
#[derive(serde::Serialize)]
struct AlbumEntry {
    id: String,
    name: String,
    artist: String,
    art_url: Option<String>,
}

/// One Spotify Connect device, serialized to Kotlin as JSON.
#[derive(serde::Serialize)]
struct DeviceEntry {
    id: String,
    name: String,
    kind: String,
    is_active: bool,
    volume_percent: Option<u8>,
    /// Whether this device accepts a remote volume change at all — some
    /// Connect receivers don't (the Web API reports this per device, not
    /// something Android can infer on its own).
    supports_volume: bool,
}

/// One row of the account's playlist tree, serialized to Kotlin as JSON
/// with a `kind` discriminator — easier to parse from `JSONArray` than
/// [spotifast_core::player::RootlistEntry]'s own externally-tagged shape,
/// where a bare `FolderEnd` would arrive as a plain JSON string sitting
/// next to object rows for the other two variants.
#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RootlistRow {
    Playlist { id: String },
    FolderStart { id: String, name: String },
    FolderEnd,
}
