//! Portable user-preference types, shared by the desktop binary and the
//! Android library.
//!
//! This holds the enums and small structs that other portable modules name,
//! with no `egui`/`fastframe`/OS coupling. The desktop-only parts of the
//! settings (theme cache, MilkDrop, the `Settings` struct that persists them,
//! and the localized `label`/`resolve` helpers) stay in the desktop crate's
//! `settings` module, which re-exports these types.

use serde::{Deserialize, Serialize};

/// Local Library identity only. Never sent to Spotify as a context URI.
pub const LIKED_SONGS_KEY: &str = "spotifast:liked-songs";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryShelf {
    #[default]
    Playlists,
    Albums,
    Artists,
    Podcasts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySort {
    Library,
    RecentlyPlayed,
    Name,
    RecentlyAdded,
    Local,
    Spotify,
}

impl LibrarySort {
    pub fn supports(self, shelf: LibraryShelf) -> bool {
        match self {
            Self::RecentlyPlayed | Self::Name | Self::Library => true,
            Self::RecentlyAdded => matches!(shelf, LibraryShelf::Albums | LibraryShelf::Podcasts),
            Self::Local | Self::Spotify => shelf == LibraryShelf::Playlists,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    Dark,
    Light,
    #[default]
    System,
}

/// Whether a Home shelf is drawn. Hidden shelves still refresh normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeShelfSettings {
    pub visible: bool,
}

impl Default for HomeShelfSettings {
    fn default() -> Self {
        Self { visible: true }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeSettings {
    pub made_for_you: HomeShelfSettings,
    pub recommendations: HomeShelfSettings,
}

/// What moves behind the player bar's controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerBarVis {
    #[default]
    Off,
    Spectrum,
    Waveform,
}

impl PlayerBarVis {
    /// The mode a click on the player bar moves to: spectrum, waveform,
    /// then off, as Winamp's visualizer cycles.
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Spectrum,
            Self::Spectrum => Self::Waveform,
            Self::Waveform => Self::Off,
        }
    }
}

/// Mini-player visualizer mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisMode {
    #[default]
    Bars,
    Scope,
    Off,
}

impl VisMode {
    /// Next mode in the display's click cycle.
    pub fn next(self) -> Self {
        match self {
            Self::Bars => Self::Scope,
            Self::Scope => Self::Off,
            Self::Off => Self::Bars,
        }
    }
}

/// The interface language: the operating system's, or one chosen in Settings.
///
/// Stored as `"system"` or a locale tag such as `"es"` or `"pt-BR"`. A file
/// without the field follows the system; a tag this version does not carry
/// also follows the system, rather than making the whole file unreadable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LanguageChoice {
    #[default]
    System,
    Locale(crate::locale::Locale),
}

impl Serialize for LanguageChoice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::System => "system",
            Self::Locale(locale) => locale.tag(),
        })
    }
}

impl<'de> Deserialize<'de> for LanguageChoice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(value
            .as_str()
            .and_then(crate::locale::Locale::from_tag)
            .map_or(Self::System, Self::Locale))
    }
}
