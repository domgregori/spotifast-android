//! Portable construction of the playback engine's configuration from the
//! user's settings.

use std::sync::Arc;

use crate::eq::{EqSettings, SharedEq};
use crate::paths::AppDirs;
use crate::player::EngineConfig;
use crate::proxy::ProxyConfig;
use crate::settings::Settings;
use crate::vis::AudioTap;

pub fn engine_config(
    dirs: &AppDirs,
    settings: &Settings,
    proxy: ProxyConfig,
    tap: Arc<AudioTap>,
    eq: SharedEq,
) -> EngineConfig {
    EngineConfig {
        tap,
        eq,
        device_name: settings.device_name.trim().to_string(),
        bitrate_kbps: settings.bitrate,
        normalisation: settings.normalisation,
        autoplay: settings.autoplay,
        gapless: settings.gapless,
        backend: settings.platform_backend(),
        buffer_ms: settings.audio_buffer_ms,
        audio_device: settings
            .audio_device
            .clone()
            .filter(|device| !device.trim().is_empty()),
        initial_volume: settings.volume,
        volume_dir: dirs.volume_dir(),
        audio_cache_dir: settings.audio_cache.then(|| dirs.audio_cache_dir()),
        audio_cache_limit: Some(settings.audio_cache_mb.max(64) * 1024 * 1024),
        proxy,
    }
}

/// The equalizer as the settings describe it.
pub fn eq_settings(settings: &Settings) -> EqSettings {
    EqSettings {
        on: settings.eq_on,
        preamp_db: settings.eq_preamp_db,
        bands_db: settings.eq_bands_db,
        balance: settings.balance,
        mono: settings.mono,
    }
    .clamped()
}
