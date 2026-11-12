//! Pinned audio: the "downloads" the Android app offers.
//!
//! librespot's on-disk audio cache is a plain size-bounded LRU with no notion
//! of a file the user asked to keep, so a track played once can be evicted
//! when the budget is tight. A pin is a small marker file written next to the
//! cached audio file, which keeps that fact on disk (it survives restarts and
//! anything that can walk the directory can see it) without teaching
//! librespot about it.
//!
//! Pinning is honest about what it is: the audio is cached, not licensed
//! offline. Spotify's real offline model has its own license renewal scheme
//! that librespot does not implement, and playback still goes through a
//! Connect session.
//!
//! On a platform where pins exist, [`sweep`] is the only thing that deletes
//! cache files, and it never deletes a pinned one. That is why such a build
//! opens its cache with librespot's own size limiter switched off: that
//! limiter cannot see pins and would happily evict one.
//!
//! Cache files sit at `<dir>/<first two hex digits of the file id>/<the rest>`
//! with no extension (librespot's own layout), so a pin for
//! `<dir>/ab/cdef` is `<dir>/ab/cdef.pin`.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The extension carried by a pin marker.
const PIN_EXT: &str = "pin";

/// Where the marker for a cached audio file lives.
pub fn pin_path(audio: &Path) -> PathBuf {
    let mut name = audio.as_os_str().to_owned();
    name.push(".pin");
    PathBuf::from(name)
}

/// The cached audio file a marker belongs to.
///
/// Only the file name is touched, and only its own UTF-8 suffix is removed, so
/// a cache directory whose *parent* path is not valid UTF-8 still resolves.
fn audio_path(marker: &Path) -> PathBuf {
    let Some(name) = marker.file_name().and_then(|name| name.to_str()) else {
        return marker.to_path_buf();
    };
    let Some(stem) = name.strip_suffix(".pin") else {
        return marker.to_path_buf();
    };
    marker.with_file_name(stem)
}

fn is_marker(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == PIN_EXT)
}

/// Whether the cached audio file at `audio` is pinned.
pub fn is_pinned(audio: &Path) -> bool {
    pin_path(audio).is_file()
}

/// Pins a cached audio file, recording which track it belongs to so the app
/// can report "downloaded" per track without re-reading librespot's ids.
///
/// The marker is written through a temporary file and renamed, so a marker is
/// never observed half-written (a rename within one directory is atomic).
pub fn pin(audio: &Path, uri: &str) -> io::Result<()> {
    let marker = pin_path(audio);
    let temporary = marker.with_extension("pin.tmp");
    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)?;
    }
    {
        let mut file = fs::File::create(&temporary)?;
        writeln!(file, "{uri}")?;
        file.sync_all()?;
    }
    fs::rename(&temporary, &marker)
}

/// Removes a cached audio file's pin, leaving the audio itself in the cache
/// (it is then ordinary eviction material again).
pub fn unpin(audio: &Path) -> io::Result<()> {
    match fs::remove_file(pin_path(audio)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// One pinned track, as recorded on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pinned {
    /// The Spotify track URI recorded when the file was pinned.
    pub uri: String,
    /// The cached audio file itself.
    pub audio: PathBuf,
    /// Its size in bytes.
    pub size: u64,
}

/// Every pinned track under `dir`, in a stable order.
///
/// Markers whose audio file has gone (evicted before pins existed, removed by
/// librespot after a decode failure, or deleted out from under the app) are
/// dropped and deleted here: a marker on its own is not a download.
pub fn pinned(dir: &Path) -> Vec<Pinned> {
    let mut found = Vec::new();
    let mut markers = Vec::new();
    collect(dir, &mut markers, &mut Vec::new());

    for marker in markers {
        let audio = audio_path(&marker);
        if !audio.is_file() {
            let _ = fs::remove_file(&marker);
            continue;
        }
        let uri = fs::read_to_string(&marker)
            .ok()
            .and_then(|text| text.lines().next().map(str::trim).map(str::to_owned))
            .filter(|line| !line.is_empty());
        let Some(uri) = uri else { continue };
        let size = fs::metadata(&audio).map(|meta| meta.len()).unwrap_or(0);
        found.push(Pinned { uri, audio, size });
    }

    found.sort_by(|a, b| a.uri.cmp(&b.uri));
    found
}

/// What a [`sweep`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// How many cache files were deleted.
    pub removed: u64,
    /// How many bytes that freed.
    pub bytes: u64,
}

/// Prunes the audio cache: files last read longer ago than `max_age` go, then
/// whole-file eviction until what remains fits `size_limit`.
///
/// Pinned files are exempt from both passes, so a download is only ever
/// removed by [`unpin`] plus a later sweep, or by clearing the cache. When
/// pins alone exceed the limit the cache is left over budget rather than
/// silently breaking the user's downloads.
pub fn sweep(dir: &Path, max_age: Option<Duration>, size_limit: Option<u64>) -> Swept {
    let mut markers = Vec::new();
    let mut audio = Vec::new();
    collect(dir, &mut markers, &mut audio);

    let mut pinned: HashSet<PathBuf> = HashSet::new();
    for marker in &markers {
        let path = audio_path(marker);
        if path.is_file() {
            pinned.insert(path);
        } else {
            let _ = fs::remove_file(marker);
        }
    }

    let cutoff = max_age.and_then(|age| SystemTime::now().checked_sub(age));
    audio.sort_by(|a, b| {
        a.accessed
            .cmp(&b.accessed)
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut result = Swept::default();
    let mut kept: Vec<Entry> = Vec::with_capacity(audio.len());
    for entry in audio {
        let stale = cutoff.is_some_and(|cutoff| entry.accessed < cutoff);
        if stale && !pinned.contains(&entry.path) {
            if fs::remove_file(&entry.path).is_ok() {
                result.removed += 1;
                result.bytes += entry.size;
            }
            continue;
        }
        kept.push(entry);
    }

    if let Some(limit) = size_limit {
        let mut total: u64 = kept.iter().map(|entry| entry.size).sum();
        for entry in &kept {
            if total <= limit {
                break;
            }
            if pinned.contains(&entry.path) {
                continue;
            }
            if fs::remove_file(&entry.path).is_ok() {
                total = total.saturating_sub(entry.size);
                result.removed += 1;
                result.bytes += entry.size;
            }
        }
    }

    result
}

struct Entry {
    path: PathBuf,
    size: u64,
    accessed: SystemTime,
}

impl Entry {
    fn new(path: PathBuf) -> Option<Self> {
        let metadata = fs::metadata(&path).ok()?;
        let accessed = metadata
            .accessed()
            .or_else(|_| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Some(Self {
            path,
            size: metadata.len(),
            accessed,
        })
    }
}

/// Walks `dir`, splitting pin markers from cached audio files. Directories
/// that cannot be read are skipped: a sweep is best-effort housekeeping and
/// must not fail the cache it is opening.
fn collect(dir: &Path, markers: &mut Vec<PathBuf>, audio: &mut Vec<Entry>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, markers, audio);
        } else if is_marker(&path) {
            markers.push(path);
        } else if let Some(entry) = Entry::new(path) {
            audio.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "spotifast-pin-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        /// A cached audio file in librespot's own layout, with `size` bytes.
        fn audio(&self, id: &str, size: usize) -> PathBuf {
            let path = self.root.join(&id[0..2]).join(&id[2..]);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![0u8; size]).unwrap();
            path
        }

        fn aged(&self, path: &Path, days: u64) {
            let when = SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
            let file = fs::File::options().write(true).open(path).unwrap();
            file.set_times(fs::FileTimes::new().set_accessed(when).set_modified(when))
                .unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn pin_round_trips_through_the_marker() {
        let fixture = Fixture::new();
        let audio = fixture.audio("abcd1234", 64);
        assert!(!is_pinned(&audio));

        pin(&audio, "spotify:track:one").unwrap();
        assert!(is_pinned(&audio));
        assert_eq!(
            pinned(&fixture.root),
            vec![Pinned {
                uri: "spotify:track:one".to_string(),
                audio: audio.clone(),
                size: 64,
            }]
        );

        unpin(&audio).unwrap();
        assert!(!is_pinned(&audio));
        assert!(pinned(&fixture.root).is_empty());
        assert!(
            audio.is_file(),
            "unpinning must leave the audio in the cache"
        );
    }

    #[test]
    fn unpinning_something_unpinned_is_not_an_error() {
        let fixture = Fixture::new();
        let audio = fixture.audio("abcd1234", 8);
        unpin(&audio).unwrap();
        unpin(&audio).unwrap();
    }

    #[test]
    fn stale_unpinned_files_go_but_pinned_ones_stay() {
        let fixture = Fixture::new();
        let old = fixture.audio("aa1111", 32);
        let old_pinned = fixture.audio("bb2222", 32);
        let fresh = fixture.audio("cc3333", 32);
        fixture.aged(&old, 40);
        fixture.aged(&old_pinned, 40);
        pin(&old_pinned, "spotify:track:kept").unwrap();

        let swept = sweep(
            &fixture.root,
            Some(Duration::from_secs(30 * 24 * 60 * 60)),
            None,
        );

        assert_eq!(swept.removed, 1);
        assert_eq!(swept.bytes, 32);
        assert!(!old.exists());
        assert!(
            old_pinned.is_file(),
            "a download must survive the age prune"
        );
        assert!(fresh.is_file());
        assert_eq!(pinned(&fixture.root).len(), 1);
    }

    #[test]
    fn the_size_pass_evicts_oldest_unpinned_first_and_never_a_pin() {
        let fixture = Fixture::new();
        let oldest = fixture.audio("aa1111", 100);
        let middle = fixture.audio("bb2222", 100);
        let newest = fixture.audio("cc3333", 100);
        let pinned_oldest = fixture.audio("dd4444", 100);
        fixture.aged(&oldest, 5);
        fixture.aged(&middle, 3);
        fixture.aged(&pinned_oldest, 9);
        fixture.aged(&newest, 1);
        pin(&pinned_oldest, "spotify:track:pinned").unwrap();

        // Room for 300 of the 400 bytes on disk: the oldest unpinned file has
        // to go even though the pinned file is older still.
        let swept = sweep(&fixture.root, None, Some(300));

        assert_eq!(swept.removed, 1);
        assert!(!oldest.exists());
        assert!(pinned_oldest.is_file());
        assert!(middle.is_file());
        assert!(newest.is_file());
    }

    #[test]
    fn pins_over_budget_are_kept_rather_than_broken() {
        let fixture = Fixture::new();
        let first = fixture.audio("aa1111", 100);
        let second = fixture.audio("bb2222", 100);
        pin(&first, "spotify:track:a").unwrap();
        pin(&second, "spotify:track:b").unwrap();

        let swept = sweep(&fixture.root, None, Some(50));

        assert_eq!(swept.removed, 0);
        assert!(first.is_file());
        assert!(second.is_file());
        assert_eq!(pinned(&fixture.root).len(), 2);
    }

    #[test]
    fn a_marker_without_its_audio_is_not_a_download() {
        let fixture = Fixture::new();
        let audio = fixture.audio("abcd1234", 16);
        pin(&audio, "spotify:track:gone").unwrap();
        fs::remove_file(&audio).unwrap();

        assert!(pinned(&fixture.root).is_empty());
        assert!(
            !pin_path(&audio).exists(),
            "the orphan marker is cleaned up"
        );
        // A sweep must not treat the orphan as a file to account for either.
        assert_eq!(sweep(&fixture.root, None, Some(0)).removed, 0);
    }

    #[test]
    fn markers_are_not_mistaken_for_audio() {
        let fixture = Fixture::new();
        let audio = fixture.audio("abcd1234", 16);
        pin(&audio, "spotify:track:x").unwrap();

        // A zero budget would delete every unpinned audio file, but the
        // marker itself must never be counted or removed as audio.
        sweep(&fixture.root, None, Some(0));
        assert!(audio.is_file());
        assert!(pin_path(&audio).is_file());
    }
}
