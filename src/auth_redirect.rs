//! The desktop's OAuth redirect capture: the core's loopback
//! `TcpListener` on `http://127.0.0.1:<port>/login`, shared with Android's
//! playback grant.

pub use spotifast_core::auth_redirect::LoopbackRedirectHandler;
