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
pub mod credentials;
pub mod eq;
pub mod file;
pub mod http;
pub mod limiter;
pub mod link;
pub mod locale;
pub mod lyrics;
pub mod made_for_you;
pub mod paths;
pub mod playlist_cover;
pub mod prerelease;
pub mod proxy;
pub mod resample;
pub mod session_reads;
pub mod settings;
pub mod util;
