//! A remote-control action, shared by the playback engine, the Connect
//! bridge, and the desktop's Sonos/device control.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAction {
    Play,
    Pause,
    Next,
    Previous,
    Seek,
    Volume,
    Shuffle,
    Repeat,
}
