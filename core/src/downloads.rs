//! What each downloaded collection holds.
//!
//! A pin says "keep this file"; it does not say why. Without a record of which
//! tracks a download covered, a collection that changes cannot be maintained:
//! an added track looks like the whole download is stale, and a removed one
//! leaves a pin that nothing will ever clear, because pinned files are exempt
//! from both the age prune and the size pass.
//!
//! So a download records the collection it belongs to and the exact tracks it
//! covered, kept beside the audio in the cache directory so that clearing the
//! cache clears this too. A collection is then downloaded, out of date, or up
//! to date by comparison with its own record, and re-downloading is a sync:
//! fetch what is missing, unpin what has left.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The record itself, kept in the audio cache directory so `Clear cache` takes
/// it along with the audio it describes.
pub const MANIFEST_FILE: &str = "downloads.json";

/// The temporary file the record is written through, so a reader never sees a
/// half-written one.
const MANIFEST_TEMP: &str = "downloads.json.tmp";

/// Whether this path is the manifest rather than cached audio. The cache's own
/// sweep walks every file in the directory, and must not treat the record as a
/// track to evict.
pub fn is_manifest(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == MANIFEST_FILE || name == MANIFEST_TEMP)
}

/// One collection's downloaded tracks, keyed by a caller-chosen identity such
/// as `playlist:<id>` or `album:<id>`.
#[derive(Clone, Debug, Default)]
pub struct Downloads {
    entries: BTreeMap<String, Vec<String>>,
}

impl Downloads {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(MANIFEST_FILE);
        let Ok(text) = fs::read_to_string(&path) else {
            return Self::default();
        };
        // A record that cannot be read is treated as absent rather than
        // fatal: the audio is still on disk and a fresh download rebuilds it.
        serde_json::from_str::<BTreeMap<String, Vec<String>>>(&text)
            .map(|entries| Self { entries })
            .unwrap_or_default()
    }

    /// Writes the record, replacing any previous one atomically.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        let text = serde_json::to_string(&self.entries)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::create_dir_all(dir)?;
        let temporary = dir.join(MANIFEST_TEMP);
        {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&temporary, dir.join(MANIFEST_FILE))
    }

    /// The tracks this collection was downloaded with, if it was.
    pub fn get(&self, key: &str) -> Option<&[String]> {
        self.entries.get(key).map(Vec::as_slice)
    }

    pub fn set(&mut self, key: &str, uris: Vec<String>) {
        self.entries.insert(key.to_string(), uris);
    }

    /// Forgets a collection, returning the tracks it held so the caller can
    /// unpin them.
    pub fn remove(&mut self, key: &str) -> Vec<String> {
        self.entries.remove(key).unwrap_or_default()
    }

    /// Every collection and its tracks, for the interface's own state.
    pub fn entries(&self) -> &BTreeMap<String, Vec<String>> {
        &self.entries
    }

    /// What a download of `wanted` has to do to bring this collection up to
    /// date: the tracks to fetch, and the pinned ones that no longer belong.
    pub fn sync(&self, key: &str, wanted: &[String]) -> (Vec<String>, Vec<String>) {
        let held = self.get(key).unwrap_or(&[]);
        let fetch: Vec<String> = wanted
            .iter()
            .filter(|uri| !held.contains(uri))
            .cloned()
            .collect();
        let drop: Vec<String> = held
            .iter()
            .filter(|uri| !wanted.contains(uri))
            .cloned()
            .collect();
        (fetch, drop)
    }
}

/// The path the record lives at, for callers that need to name it (cache
/// clearing, tests).
pub fn manifest_path(dir: &Path) -> PathBuf {
    dir.join(MANIFEST_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "spotifast-downloads-tests-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn uris(items: &[&str]) -> Vec<String> {
        items.iter().map(|uri| uri.to_string()).collect()
    }

    #[test]
    fn a_record_round_trips() {
        let fixture = Fixture::new();
        let mut downloads = Downloads::default();
        downloads.set(
            "playlist:one",
            uris(&["spotify:track:a", "spotify:track:b"]),
        );
        downloads.save(&fixture.dir).unwrap();

        let loaded = Downloads::load(&fixture.dir);
        assert_eq!(
            loaded.get("playlist:one").unwrap(),
            uris(&["spotify:track:a", "spotify:track:b"]).as_slice()
        );
    }

    #[test]
    fn an_unreadable_record_is_treated_as_no_record() {
        let fixture = Fixture::new();
        fs::write(fixture.dir.join(MANIFEST_FILE), b"not json at all").unwrap();
        assert!(Downloads::load(&fixture.dir).get("playlist:one").is_none());
    }

    #[test]
    fn a_missing_record_is_treated_as_no_record() {
        let fixture = Fixture::new();
        assert!(Downloads::load(&fixture.dir).entries().is_empty());
    }

    #[test]
    fn saving_leaves_no_temporary_behind() {
        let fixture = Fixture::new();
        let mut downloads = Downloads::default();
        downloads.set("album:one", uris(&["spotify:track:a"]));
        downloads.save(&fixture.dir).unwrap();
        assert!(!fixture.dir.join(MANIFEST_TEMP).exists());
        assert!(fixture.dir.join(MANIFEST_FILE).is_file());
    }

    #[test]
    fn sync_reports_what_to_fetch_and_what_to_drop() {
        let fixture = Fixture::new();
        let mut downloads = Downloads::default();
        downloads.set(
            "playlist:one",
            uris(&["spotify:track:a", "spotify:track:b"]),
        );

        // `b` left the playlist, `c` joined it.
        let (fetch, drop) = downloads.sync(
            "playlist:one",
            &uris(&["spotify:track:a", "spotify:track:c"]),
        );
        assert_eq!(fetch, uris(&["spotify:track:c"]));
        assert_eq!(drop, uris(&["spotify:track:b"]));
    }

    #[test]
    fn an_unchanged_collection_needs_nothing() {
        let downloads = Downloads::default();
        let (fetch, drop) = downloads.sync("album:x", &uris(&["spotify:track:a"]));
        assert_eq!(fetch, uris(&["spotify:track:a"]));
        assert!(drop.is_empty());
    }

    #[test]
    fn removing_returns_the_tracks_to_unpin() {
        let mut downloads = Downloads::default();
        downloads.set("album:x", uris(&["spotify:track:a"]));
        assert_eq!(downloads.remove("album:x"), uris(&["spotify:track:a"]));
        assert!(downloads.get("album:x").is_none());
        assert!(downloads.remove("album:x").is_empty());
    }

    #[test]
    fn the_manifest_is_not_mistaken_for_audio() {
        assert!(is_manifest(&manifest_path(Path::new("/cache"))));
        assert!(is_manifest(&Path::new("/cache/downloads.json.tmp")));
        assert!(!is_manifest(&Path::new("/cache/ab/cdef")));
    }
}
