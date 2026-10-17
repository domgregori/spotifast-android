//! Desktop-only entry points on the core `Backend`. The native file dialog
//! for choosing a playlist cover stays here, behind `rfd`, instead of in core.

use std::future::Future;
use std::pin::Pin;

use spotifast_core::backend::{Backend, Command};

pub trait BackendExt {
    fn choose_playlist_cover(&self, id: String, request: u64);
}

impl BackendExt for Backend {
    /// Open the native cover dialog on the UI thread, then hand the chosen
    /// path to the runtime. Skipped while offline, as before.
    fn choose_playlist_cover(&self, id: String, request: u64) {
        if self.is_offline() {
            return;
        }
        let selected: Pin<Box<dyn Future<Output = Option<std::path::PathBuf>> + Send>> =
            Box::pin(async move {
                rfd::AsyncFileDialog::new()
                    .set_title("Choose playlist cover")
                    .add_filter("JPEG or PNG image", &["jpg", "jpeg", "png"])
                    .pick_file()
                    .await
                    .map(|file| file.path().to_path_buf())
            });
        self.send(Command::ChoosePlaylistCover {
            id,
            request,
            selected,
        });
    }
}
