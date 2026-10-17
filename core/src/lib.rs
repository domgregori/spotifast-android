//! Spotifast's portable core, shared by the desktop binary and the Android
//! library.
//!
//! Portable modules move here from the desktop crate (see HANDOFF.md Phase A)
//! and are re-exported by the desktop crate so its build and behaviour stay
//! unchanged. Modules that touch `egui`/`eframe`/`fastframe-*` or the desktop
//! OS stay in the desktop crate.

pub mod auth;
pub mod api;
pub mod account_cache;
pub mod backend;
pub mod cast;
pub mod config;
pub mod credentials;
pub mod eq;
pub mod file;
pub mod history;
pub mod http;
pub mod liked;
pub mod limiter;
pub mod link;
pub mod locale;
pub mod lyrics;
pub mod made_for_you;
pub mod model;
pub mod now_playing;
pub mod opener;
pub mod paths;
pub mod player;
pub mod playlist_cover;
pub mod prerelease;
pub mod proxy;
pub mod remote_action;
pub mod repeat;
pub mod resample;
pub mod services;
pub mod session_reads;
#[cfg(not(target_os = "android"))]
pub mod shm;
pub mod settings;
pub mod sink;
pub mod sonos;
pub mod updates;
pub mod util;
pub mod vis;
pub mod wake;
pub mod zeroconf;
