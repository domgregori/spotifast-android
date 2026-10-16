//! The playing item as the interface sees it, whichever device plays it,
//! shared by the desktop binary and the Android library.

use crate::api::models::ArtistRef;
use crate::repeat::RepeatMode;

#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    pub local: bool,
    pub device_name: Option<String>,
    /// Spotify's device type, such as "Smartphone", for the device's icon.
    pub device_kind: Option<String>,
    pub uri: String,
    pub id: Option<String>,
    pub title: String,
    pub artists: Vec<ArtistRef>,
    pub subtitle: String,
    pub album_name: String,
    pub album_id: Option<String>,
    pub show_id: Option<String>,
    pub art_url: Option<String>,
    pub art_small: Option<String>,
    pub duration_ms: u32,
    pub position_ms: u32,
    pub playing: bool,
    pub loading: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub volume_percent: u8,
    pub can_control: bool,
    /// Whether Spotify lets this app change the device's volume. A phone
    /// playing over Bluetooth, for one, reports that it does not.
    pub can_set_volume: bool,
    pub is_episode: bool,
    /// The remembered song from the last session, shown paused before a
    /// first press. Nothing is playing yet.
    pub resuming: bool,
}
