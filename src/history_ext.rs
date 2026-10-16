//! Desktop persistence for the portable play history. The gettext-free data
//! model lives in `spotifast_core::history`; handing it to the app's
//! fastframe-file writer stays here.

use std::path::Path;
use std::sync::atomic::Ordering;

use spotifast_core::history::History;

pub trait HistoryExt {
    fn save(&mut self, files: &fastframe_file::Writer, path: &Path);
}

impl HistoryExt for History {
    /// Hands the history to `files` if it changed since the last save. A
    /// write that fails leaves it to be written by the next save.
    fn save(&mut self, files: &fastframe_file::Writer, path: &Path) {
        let failed = self.failed_flag().swap(false, Ordering::SeqCst);
        if !self.take_dirty() && !failed {
            return;
        }
        match serde_json::to_string(self.plays()) {
            Ok(text) => {
                // Replaced whole, as settings are: the history is written
                // when the session ends, and a shutdown that cuts the write
                // short must not leave half a file behind. The writer waits
                // until the new file and its move reach the disk, off the UI
                // thread, so a power cut cannot leave an empty history.
                // What someone listened to is theirs: on Unix the file is
                // readable by its owner only, whatever the old one allowed.
                let failed = self.failed_flag();
                files.write_or(path, text, fastframe_file::Access::Private, move || {
                    failed.store(true, Ordering::SeqCst)
                });
            }
            Err(error) => {
                self.mark_dirty();
                log::warn!("could not write the play history: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastframe_file::Writer;
    use spotifast_core::api::models::Track;
    use std::time::Duration;

    /// A save replaces the whole file and leaves no temporary behind, and a
    /// second save over it reads back the newer history.
    #[test]
    fn a_saved_history_reads_back_whole() {
        let dir =
            std::env::temp_dir().join(format!("spotifast-history-{:016x}", rand::random::<u64>()));
        let path = dir.join("history.json");
        let at: jiff::Timestamp = "2026-09-01T09:00:00Z".parse().unwrap();
        let files = Writer::new();
        let mut history = History::default();
        for uri in ["spotify:track:a", "spotify:track:b"] {
            history.record(
                Track {
                    uri: uri.into(),
                    ..Track::default()
                },
                at,
            );
            history.save(&files, &path);
        }
        assert!(files.flush(Duration::from_secs(10)));
        let read = History::load(&path);
        let uris: Vec<_> = read
            .plays()
            .iter()
            .map(|play| play.track.uri.as_str())
            .collect();
        assert_eq!(uris, ["spotify:track:b", "spotify:track:a"]);
        assert!(crate::util::temporary_files(path.parent().unwrap()).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A save that cannot write keeps the plays to write, so the next save
    /// still has them rather than losing the song just recorded.
    #[test]
    fn a_failed_save_keeps_the_history_to_write() {
        let dir =
            std::env::temp_dir().join(format!("spotifast-history-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        // A file where the history's folder should be: nothing can be
        // written under it.
        let blocked = dir.join("blocked");
        std::fs::write(&blocked, "").unwrap();
        let mut history = History::default();
        history.record(
            Track {
                uri: "spotify:track:a".into(),
                ..Track::default()
            },
            "2026-09-01T09:00:00Z".parse().unwrap(),
        );
        let files = Writer::new();
        history.save(&files, &blocked.join("history.json"));
        assert!(files.flush(Duration::from_secs(10)));
        assert!(history.needs_save(), "the failed write is still to be done");

        let path = dir.join("history.json");
        history.save(&files, &path);
        assert!(files.flush(Duration::from_secs(10)));
        assert!(!history.needs_save());
        assert_eq!(History::load(&path).plays().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The history is readable by its owner only, even when it replaces a
    /// file others could read.
    #[cfg(unix)]
    #[test]
    fn the_history_is_kept_from_other_accounts() {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("spotifast-history-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        std::fs::write(&path, "[]").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut history = History::default();
        history.record(
            Track {
                uri: "spotify:track:a".into(),
                ..Track::default()
            },
            "2026-09-01T09:00:00Z".parse().unwrap(),
        );
        let files = Writer::new();
        history.save(&files, &path);
        assert!(files.flush(Duration::from_secs(10)));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
