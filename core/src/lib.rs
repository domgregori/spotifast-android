//! Spotifast's portable core, shared by the desktop binary and the Android
//! library.
//!
//! Currently an empty shell: the portable modules from the desktop crate
//! (`src/auth.rs`, `src/player.rs`, `src/sink.rs`, `src/backend.rs`,
//! `src/api/`, and the rest listed in HANDOFF.md Phase A) move here in the
//! following steps, with the desktop crate re-exporting them so its build and
//! behaviour stay unchanged throughout.
