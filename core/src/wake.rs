//! The interface wake the backend calls when its state changes, so the
//! front end repaints. On desktop this is the egui context; on Android it
//! becomes a `MutableStateFlow` collected by Compose.

/// Asks the front end for a new frame (or the equivalent) because the
/// backend's state changed.
pub trait Wake: Send + Sync {
    fn wake(&self);
}

/// A wake that does nothing, for tests and for backends with no live UI.
#[derive(Default)]
pub struct NoWake;

impl Wake for NoWake {
    fn wake(&self) {}
}
