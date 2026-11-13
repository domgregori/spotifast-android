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

use librespot_audio::{AudioDecrypt, AudioFile};
use librespot_core::{FileId, SpotifyId, SpotifyUri, cache::Cache};
use librespot_metadata::audio::{AudioFileFormat, AudioFiles, AudioItem};
use librespot_playback::config::Bitrate;

use crate::player::Engine;

/// How long to wait for librespot to finish writing a completed download into
/// the cache. The save runs in its own task once the fetch completes, so it is
/// normally a moment behind the last read.
const CACHE_WRITE_TIMEOUT: Duration = Duration::from_secs(20);

/// Waits between attempts at one track. Spotify rate-limits audio keys, and
/// the second wait is long enough for that limit to clear.
const DOWNLOAD_RETRY_DELAY: Duration = Duration::from_secs(5);
const DOWNLOAD_RETRY_LONG_DELAY: Duration = Duration::from_secs(20);

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

        // Spotify rate-limits audio keys, and the metadata and CDN lookups go
        // over the same session; a batch of a hundred tracks will meet that
        // limit. One failure used to abandon the track for good, which left a
        // "downloaded" album with silent holes in it. Retry the fetch a couple
        // of times, waiting longer each time, and report only the last error.
        for (attempt, delay) in [(1u32, DOWNLOAD_RETRY_DELAY), (2, DOWNLOAD_RETRY_LONG_DELAY)] {
            match self.download_once(cache, uri).await {
                Ok(fetched) => return Ok(fetched),
                Err(error) => {
                    log::warn!("download of <{uri}> failed on attempt {attempt}: {error}");
                    tokio::time::sleep(delay).await;
                }
            }
        }
        self.download_once(cache, uri).await
    }

    /// One attempt at fetching and pinning a track.
    async fn download_once(&self, cache: &Cache, uri: &str) -> Result<bool, String> {
        let track_uri = SpotifyUri::from_uri(uri).map_err(|error| format!("{error}"))?;
        let audio_item = AudioItem::get_file(self.session(), track_uri)
            .await
            .map_err(|error| format!("unable to load <{uri}>: {error}"))?;
        let (format, file_id) = pick_file(&audio_item.files, self.bitrate())
            .ok_or_else(|| format!("<{}> has no file this engine can play", audio_item.name))?;

        let path = cache
            .file_path(file_id)
            .ok_or_else(|| "no audio cache path".to_string())?;

        // The cache holds ciphertext, which only librespot's player can use
        // with a live session. A download therefore also writes a decrypted
        // copy, so the track can be played with no network at all.
        let track_id: SpotifyId = (&audio_item.track_id)
            .try_into()
            .map_err(|error| format!("unable to identify <{uri}>: {error}"))?;
        // The cache file lives at <cache>/audio/<shard>/<name>, so the cache
        // root is three levels up.
        let offline_dir = path
            .parent()
            .and_then(|shard| shard.parent())
            .and_then(|audio| audio.parent())
            .map(|cache| cache.join("offline"))
            .ok_or_else(|| "no offline audio directory".to_string())?;
        let offline = offline_dir.join(format!(
            "{}.{}",
            audio_item.track_id.to_id(),
            extension(format)
        ));

        let fetched = if path.is_file() {
            // Downloaded before decrypted copies existed: make one from the
            // cache, which needs no network.
            if !offline.is_file() {
                self.decrypt_from_cache(track_id, file_id, &offline).await?;
            }
            false
        } else {
            self.fetch_file(track_id, file_id, format, &offline).await?;
            wait_for_cache(cache, file_id).await?;
            true
        };

        crate::pins::pin(&path, uri).map_err(|error| format!("unable to pin {uri}: {error}"))?;
        Ok(fetched)
    }

    /// Runs librespot's fetch for one file to completion, which is what makes
    /// it land in the cache, and writes the decrypted copy as it goes: the
    /// same bytes the player would decrypt, kept for offline playback.
    async fn fetch_file(
        &self,
        track_id: SpotifyId,
        file_id: FileId,
        format: AudioFileFormat,
        offline: &Path,
    ) -> Result<(), String> {
        let rate = stream_data_rate(format);
        let key = self
            .session()
            .audio_key()
            .request(track_id, file_id)
            .await
            .map_err(|error| format!("unable to fetch the audio key for {file_id}: {error}"))?;
        // `open_for_download` asks for the whole file at once; the streaming
        // open would move one playback read-ahead window per round trip, which
        // on a high-latency link is a small fraction of the link's throughput.
        let file = AudioFile::open_for_download(self.session(), file_id, rate)
            .await
            .map_err(|error| format!("unable to fetch {file_id}: {error}"))?;
        let mut file = AudioDecrypt::new(Some(key), file);

        // Reading is blocking by design (the player reads it on its own
        // thread), so it must not sit on a runtime worker.
        write_atomically(offline, move |out| copy_audio(&mut file, out))
            .await
            .map_err(|error| format!("unable to download {file_id}: {error}"))
    }

    /// Turns an already cached track into its decrypted copy. Used for
    /// downloads made before decrypted copies were kept, and needs the audio
    /// key but no CDN request.
    async fn decrypt_from_cache(
        &self,
        track_id: SpotifyId,
        file_id: FileId,
        offline: &Path,
    ) -> Result<(), String> {
        let cached = self
            .session()
            .cache()
            .and_then(|cache| cache.file(file_id))
            .ok_or_else(|| format!("{file_id} is not in the cache"))?;
        let key = self
            .session()
            .audio_key()
            .request(track_id, file_id)
            .await
            .map_err(|error| format!("unable to fetch the audio key for {file_id}: {error}"))?;
        let mut cached = AudioDecrypt::new(Some(key), cached);
        write_atomically(offline, move |out| copy_audio(&mut cached, out))
            .await
            .map_err(|error| format!("unable to write the offline copy of {file_id}: {error}"))
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

/// The file extension for a decrypted copy, so a player can choose its decoder
/// from the name.
fn extension(format: AudioFileFormat) -> &'static str {
    use AudioFileFormat::*;
    match format {
        OGG_VORBIS_96 | OGG_VORBIS_160 | OGG_VORBIS_320 => "ogg",
        MP3_256 | MP3_320 | MP3_160 | MP3_96 | MP3_160_ENC => "mp3",
        FLAC_FLAC | FLAC_FLAC_24BIT => "flac",
        AAC_24 | AAC_48 | AAC_160 | AAC_320 | XHE_AAC_24 | XHE_AAC_16 | XHE_AAC_12 | MP4_128
        | OTHER5 => "m4a",
    }
}

/// Copies the decrypted stream into `out`, leaving out the one page Spotify
/// puts before the audio.
///
/// That page decrypts to bytes that are not audio, and a standard Ogg reader
/// expects the identification header on the first page, so a copy that keeps
/// it plays in Spotify's own client but not in `ffmpeg` or an Ogg extractor.
/// A stream whose first page *is* the identification header is left alone.
fn copy_audio<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    out: &mut W,
) -> std::io::Result<()> {
    use std::io::Read as _;

    let mut first = [0u8; 27];
    let mut filled = 0;
    while filled < first.len() {
        let read = reader.read(&mut first[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    if filled < first.len() {
        // Too short to be a page at all: keep what there is.
        out.write_all(&first[..filled])?;
        return Ok(());
    }
    if &first[0..4] != b"OggS" {
        // Not the layout this knows about; keep everything.
        out.write_all(&first)?;
        return std::io::copy(reader, out).map(|_| ());
    }
    let segments = first[26] as usize;
    let mut table = vec![0u8; segments];
    reader.read_exact(&mut table)?;
    let body_len: usize = table.iter().map(|&length| length as usize).sum();
    let mut body = vec![0u8; body_len];
    reader.read_exact(&mut body)?;

    let identification = body.starts_with(b"\x01vorbis")
        || body.starts_with(b"OpusHead")
        || body.starts_with(b"fLaC");
    if identification {
        out.write_all(&first)?;
        out.write_all(&table)?;
        out.write_all(&body)?;
    }
    std::io::copy(reader, out).map(|_| ())
}

/// Writes through `<name>.part` and renames, so a download killed halfway
/// cannot leave a file that looks playable but is cut short.
async fn write_atomically<F>(path: &Path, write: F) -> Result<(), String>
where
    F: FnOnce(&mut std::fs::File) -> std::io::Result<()> + Send + 'static,
{
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut temporary = path.clone();
        let name = temporary
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        temporary.set_file_name(format!("{name}.part"));
        let mut out = std::fs::File::create(&temporary)?;
        write(&mut out)?;
        out.sync_all()?;
        drop(out);
        std::fs::rename(&temporary, &path)
    })
    .await
    .map_err(|error| format!("write task failed: {error}"))?
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod copy_tests {
    use super::copy_audio;
    use std::io::Cursor;

    /// One Ogg page carrying `body`, with a valid page header and segment
    /// table but no meaningful checksum: the copy code never reads it.
    fn page(body: &[u8]) -> Vec<u8> {
        let mut segments = Vec::new();
        let mut remaining = body.len();
        while remaining >= 255 {
            segments.push(255u8);
            remaining -= 255;
        }
        segments.push(remaining as u8);
        let mut out = b"OggS".to_vec();
        out.resize(26, 0);
        out.push(segments.len() as u8);
        out.extend_from_slice(&segments);
        out.extend_from_slice(body);
        out
    }

    fn copied(stream: &[u8]) -> Vec<u8> {
        let mut reader = Cursor::new(stream.to_vec());
        let mut out = Vec::new();
        copy_audio(&mut reader, &mut out).unwrap();
        out
    }

    #[test]
    fn the_spotify_preamble_is_left_out() {
        // What Spotify sends first: one page that decrypts to bytes that are
        // not audio, before the Vorbis identification header.
        let mut stream = page(&[0xAB; 139]);
        let identification = page(b"\x01vorbis identification header");
        stream.extend_from_slice(&identification);
        stream.extend_from_slice(&page(b"audio"));

        let out = copied(&stream);
        assert!(out.starts_with(b"OggS"));
        assert!(out.windows(7).any(|window| window == b"\x01vorbis"));
        assert!(!out.windows(2).any(|window| window == [0xAB, 0xAB]));
    }

    #[test]
    fn a_stream_that_is_already_audio_is_copied_whole() {
        let mut stream = page(b"\x01vorbis identification header");
        stream.extend_from_slice(&page(b"audio"));
        assert_eq!(copied(&stream), stream);
    }

    #[test]
    fn something_that_is_not_ogg_is_not_lost() {
        let stream = b"not an ogg stream at all".to_vec();
        assert_eq!(copied(&stream), stream);
    }
}
