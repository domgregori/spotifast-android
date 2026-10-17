//! The desktop implementation of the backend's services: album-art fetching
//! through the egui ArtLoader.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use spotifast_core::services::Services;

use crate::images::ArtLoader;

pub struct DesktopServices {
    pub art: ArtLoader,
}

impl Services for DesktopServices {
    fn fetch_art(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<[u8]>, String>> + Send + '_>> {
        let url = url.to_owned();
        Box::pin(async move { self.art.fetch(&url).await })
    }
}
