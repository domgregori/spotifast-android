//! Spotifast's portable core, exposed to Kotlin over UniFFI.

mod auth_redirect;
mod credential_store;
mod wake;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use spotifast_core::backend::{ApiRequest, ApiResponse, AuthStatus, Backend, Command, Event};
use spotifast_core::config;
use spotifast_core::http::Http;
use spotifast_core::paths::AppDirs;
use spotifast_core::proxy::ProxyConfig;
use spotifast_core::services::{NoServices, Services};
use spotifast_core::settings::Settings;
use spotifast_core::vis::AudioTap;
use spotifast_core::wake::Wake;

use auth_redirect::AndroidAuthRedirect;
use credential_store::AndroidProtectedStore;
use wake::AndroidWake;

uniffi::setup_scaffolding!("spotifast");

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

/// The long-lived core handle: owns the backend (and its tokio runtime) for
/// the process lifetime.
#[derive(uniffi::Object)]
pub struct CoreHandle {
    backend: Arc<Mutex<Backend>>,
    redirect: Arc<AndroidAuthRedirect>,
    listener: Mutex<Option<Arc<dyn PlaybackStateListener>>>,
    pump_started: AtomicBool,
    wake_rx: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    library_generation: AtomicU64,
    web_client_id: String,
}

#[uniffi::export]
impl CoreHandle {
    #[uniffi::constructor]
    pub fn new(web_client_id: String, credential_store: Arc<dyn CredentialStore>) -> Arc<Self> {
        let dirs = AppDirs::discover();
        let _ = dirs.ensure();
        let settings = Settings::default();
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
            library_generation: AtomicU64::new(0),
            web_client_id,
        })
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

    /// Request the account's playlist list; the result arrives via
    /// `on_library`.
    pub fn library(&self) {
        let generation = self.library_generation.fetch_add(1, Ordering::Relaxed);
        self.backend.lock().unwrap().api(ApiRequest::MyPlaylists {
            offset: 0,
            generation,
        });
    }

    /// Register the outbound-state listener and start the event pump. Calling
    /// this again replaces the listener.
    pub fn set_listener(&self, listener: Arc<dyn PlaybackStateListener>) {
        *self.listener.lock().unwrap() = Some(listener.clone());

        if self.pump_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let backend = self.backend.clone();
        let wake_rx = self
            .wake_rx
            .lock()
            .unwrap()
            .take()
            .expect("the wake receiver is taken once");
        std::thread::Builder::new()
            .name("spotifast-android-events".to_string())
            .spawn(move || {
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
                            Event::Api(api) => {
                                if let ApiResponse::MyPlaylists { result, .. } = *api {
                                    let entries: Vec<LibraryEntry> = match result {
                                        Ok(page) => page
                                            .items
                                            .into_iter()
                                            .map(|playlist| LibraryEntry {
                                                id: playlist.id,
                                                name: playlist.name,
                                            })
                                            .collect(),
                                        Err(_) => Vec::new(),
                                    };
                                    if let Ok(json) = serde_json::to_string(&entries) {
                                        listener.on_library(json);
                                    }
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

/// One playlist in the bare library list, serialized to Kotlin as JSON.
#[derive(serde::Serialize)]
struct LibraryEntry {
    id: String,
    name: String,
}
