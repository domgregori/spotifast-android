//! Warming the audio cache for tracks the user asked to keep.
//!
//! A "download" here is exactly what it says: librespot's own fetch is run to
//! completion for the track's file, which writes it into the on-disk cache,
//! and the file is then pinned so eviction leaves it alone. Nothing is
//! licensed and nothing bypasses the session; see [`crate::pins`] for what
//! that does and does not buy.
//!
//! The fetch is the same one playback uses, so a downloaded track is the very
//! file the player would have streamed, at the quality this engine is
//! configured for, and playing it afterwards needs no CDN request.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use librespot_audio::AudioFile;
use librespot_core::{FileId, SpotifyUri, cache::Cache};
use librespot_metadata::audio::{AudioFileFormat, AudioFiles, AudioItem};
use librespot_playback::config::Bitrate;

use crate::player::Engine;

/// How long to wait for librespot to finish writing a completed download into
/// the cache. The save runs in its own task once the fetch completes, so it is
/// normally a moment behind the last read.
const CACHE_WRITE_TIMEOUT: Duration = Duration::from_secs(20);

impl Engine {
    /// Downloads one track into the audio cache and pins it.
    ///
    /// Returns `true` when the track was fetched now, `false` when it was
    /// already cached (nothing to download, it just gets pinned). Errors are
    /// the caller's to report per track: one unavailable track should not
    /// abandon the rest of an album.
    pub async fn download_track(&self, uri: &str) -> Result<bool, String> {
        let cache = self
            .session()
            .cache()
            .ok_or_else(|| "no audio cache".to_string())?;

        let track_uri = SpotifyUri::from_uri(uri).map_err(|error| format!("{error}"))?;
        let audio_item = AudioItem::get_file(self.session(), track_uri)
            .await
            .map_err(|error| format!("unable to load <{uri}>: {error}"))?;
        let (format, file_id) = pick_file(&audio_item.files, self.bitrate())
            .ok_or_else(|| format!("<{}> has no file this engine can play", audio_item.name))?;

        let path = cache
            .file_path(file_id)
            .ok_or_else(|| "no audio cache path".to_string())?;

        let fetched = if path.is_file() {
            false
        } else {
            self.fetch_file(file_id, format).await?;
            wait_for_cache(cache, file_id).await?;
            true
        };

        crate::pins::pin(&path, uri).map_err(|error| format!("unable to pin {uri}: {error}"))?;
        Ok(fetched)
    }

    /// Runs librespot's fetch for one file to completion, which is what makes
    /// it land in the cache.
    async fn fetch_file(&self, file_id: FileId, format: AudioFileFormat) -> Result<(), String> {
        let rate = stream_data_rate(format);
        let file = AudioFile::open(self.session(), file_id, rate)
            .await
            .map_err(|error| format!("unable to fetch {file_id}: {error}"))?;

        // Reading is blocking by design (the player reads it on its own
        // thread), so it must not sit on a runtime worker.
        tokio::task::spawn_blocking(move || {
            let mut file = file;
            let mut sink = std::io::sink();
            std::io::copy(&mut file, &mut sink).map(|_| ())
        })
        .await
        .map_err(|error| format!("download task failed: {error}"))?
        .map_err(|error| format!("unable to download {file_id}: {error}"))
    }
}

/// The file to download for `bitrate`: the best format this build can play at
/// or below the configured quality, preferring Ogg Vorbis.
///
/// This mirrors librespot's own player, which is where the order comes from;
/// it is repeated here only because that selection is private to the playback
/// crate and a download should land on the file playback would have used.
fn pick_file(files: &AudioFiles, bitrate: Bitrate) -> Option<(AudioFileFormat, FileId)> {
    let order: [AudioFileFormat; 7] = match bitrate {
        Bitrate::Bitrate96 => [
            AudioFileFormat::OGG_VORBIS_96,
            AudioFileFormat::MP3_96,
            AudioFileFormat::OGG_VORBIS_160,
            AudioFileFormat::MP3_160,
            AudioFileFormat::MP3_256,
            AudioFileFormat::OGG_VORBIS_320,
            AudioFileFormat::MP3_320,
        ],
        Bitrate::Bitrate160 => [
            AudioFileFormat::OGG_VORBIS_160,
            AudioFileFormat::MP3_160,
            AudioFileFormat::OGG_VORBIS_96,
            AudioFileFormat::MP3_96,
            AudioFileFormat::MP3_256,
            AudioFileFormat::OGG_VORBIS_320,
            AudioFileFormat::MP3_320,
        ],
        _ => [
            AudioFileFormat::OGG_VORBIS_320,
            AudioFileFormat::MP3_320,
            AudioFileFormat::MP3_256,
            AudioFileFormat::OGG_VORBIS_160,
            AudioFileFormat::MP3_160,
            AudioFileFormat::OGG_VORBIS_96,
            AudioFileFormat::MP3_96,
        ],
    };
    order
        .into_iter()
        .find_map(|format| files.get(&format).map(|id| (format, *id)))
}

/// Bytes per second the fetch should aim for, as librespot's player computes
/// it. The fetch uses this to pace itself; it does not change what is stored.
fn stream_data_rate(format: AudioFileFormat) -> usize {
    let kbps: f32 = match format {
        AudioFileFormat::OGG_VORBIS_96 => 12.,
        AudioFileFormat::OGG_VORBIS_160 => 20.,
        AudioFileFormat::OGG_VORBIS_320 => 40.,
        AudioFileFormat::MP3_256 => 32.,
        AudioFileFormat::MP3_320 => 40.,
        AudioFileFormat::MP3_160 => 20.,
        AudioFileFormat::MP3_96 => 12.,
        AudioFileFormat::MP3_160_ENC => 20.,
        AudioFileFormat::AAC_24 => 3.,
        AudioFileFormat::AAC_48 => 6.,
        AudioFileFormat::AAC_160 => 20.,
        AudioFileFormat::AAC_320 => 40.,
        AudioFileFormat::MP4_128 => 16.,
        AudioFileFormat::OTHER5 => 40.,
        AudioFileFormat::FLAC_FLAC => 112.,
        AudioFileFormat::XHE_AAC_12 => 1.5,
        AudioFileFormat::XHE_AAC_16 => 2.,
        AudioFileFormat::XHE_AAC_24 => 3.,
        AudioFileFormat::FLAC_FLAC_24BIT => 3.,
    };
    (kbps * 1024.).ceil() as usize
}

/// Waits for librespot to write the finished download into the cache.
async fn wait_for_cache(cache: &Cache, file_id: FileId) -> Result<PathBuf, String> {
    let path = cache
        .file_path(file_id)
        .ok_or_else(|| "no audio cache path".to_string())?;
    let started = Instant::now();
    while !path.is_file() {
        if started.elapsed() > CACHE_WRITE_TIMEOUT {
            return Err(format!("{file_id} finished downloading but was not cached"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(path)
}

/// Whether `dir` already holds a usable download for `uri`.
pub fn is_downloaded(dir: &Path, uri: &str) -> bool {
    crate::pins::pinned(dir).iter().any(|pin| pin.uri == uri)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn files(formats: &[AudioFileFormat]) -> AudioFiles {
        let mut map = HashMap::new();
        for (index, format) in formats.iter().enumerate() {
            map.insert(*format, FileId([index as u8; 20]));
        }
        AudioFiles(map)
    }

    #[test]
    fn the_highest_quality_pick_prefers_vorbis_then_mp3() {
        let both = files(&[AudioFileFormat::OGG_VORBIS_320, AudioFileFormat::MP3_320]);
        assert_eq!(
            pick_file(&both, Bitrate::Bitrate320).unwrap().0,
            AudioFileFormat::OGG_VORBIS_320
        );

        let mp3_only = files(&[AudioFileFormat::MP3_320]);
        assert_eq!(
            pick_file(&mp3_only, Bitrate::Bitrate320).unwrap().0,
            AudioFileFormat::MP3_320
        );
    }

    #[test]
    fn a_lower_bitrate_never_picks_a_higher_one_when_a_lower_exists() {
        let all = files(&[
            AudioFileFormat::OGG_VORBIS_320,
            AudioFileFormat::OGG_VORBIS_160,
            AudioFileFormat::OGG_VORBIS_96,
        ]);
        assert_eq!(
            pick_file(&all, Bitrate::Bitrate96).unwrap().0,
            AudioFileFormat::OGG_VORBIS_96
        );
        assert_eq!(
            pick_file(&all, Bitrate::Bitrate160).unwrap().0,
            AudioFileFormat::OGG_VORBIS_160
        );
    }

    #[test]
    fn a_track_with_only_high_quality_files_falls_back_to_them() {
        // Podcasts and some catalogue entries are only served at 320.
        let high_only = files(&[AudioFileFormat::OGG_VORBIS_320]);
        assert_eq!(
            pick_file(&high_only, Bitrate::Bitrate96).unwrap().0,
            AudioFileFormat::OGG_VORBIS_320
        );
    }

    #[test]
    fn nothing_playable_is_reported_as_nothing() {
        assert!(pick_file(&files(&[]), Bitrate::Bitrate320).is_none());
    }

    #[test]
    fn the_fetch_is_paced_at_the_formats_own_rate() {
        assert_eq!(stream_data_rate(AudioFileFormat::OGG_VORBIS_320), 40 * 1024);
        assert_eq!(stream_data_rate(AudioFileFormat::OGG_VORBIS_96), 12 * 1024);
    }
}
