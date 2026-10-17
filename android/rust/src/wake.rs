//! The backend's wake, signalled to the event-pump thread.

use spotifast_core::wake::Wake;

/// Signals the event-pump thread whenever the backend's state changes.
pub struct AndroidWake {
    pub signal: std::sync::mpsc::Sender<()>,
}

impl Wake for AndroidWake {
    fn wake(&self) {
        let _ = self.signal.send(());
    }
}
