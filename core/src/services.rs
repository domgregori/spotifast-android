//! The backend's desktop integrations, injected by the front end.
//!
//! The Android front end supplies its own implementations (no-ops or
//! platform calls); the desktop supplies the egui/OS-backed ones.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub trait Services: Send + Sync {
    /// Fetches album-art bytes for `url`, for the accent colour and playlist
    /// cover checks. Android returns an error: Compose loads art itself.
    fn fetch_art(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<[u8]>, String>> + Send + '_>>;
}

/// A services handle with no desktop integrations, for tests and headless
/// runs.
pub struct NoServices;

impl Services for NoServices {
    fn fetch_art(
        &self,
        _url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<[u8]>, String>> + Send + '_>> {
        Box::pin(async { Err("no artwork loader".into()) })
    }
}

/// Fetches artwork straight over the network, for tests that serve their own
/// image bytes. The desktop's ArtLoader caches; this does not.
#[cfg(test)]
pub(crate) struct HttpServices {
    http: crate::http::Http,
}

#[cfg(test)]
impl HttpServices {
    pub(crate) fn new(http: crate::http::Http) -> Self {
        Self { http }
    }
}

#[cfg(test)]
impl Services for HttpServices {
    fn fetch_art(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<[u8]>, String>> + Send + '_>> {
        let url = url.to_owned();
        let http = self.http.clone();
        Box::pin(async move {
            let bytes = http
                .client()?
                .get(&url)
                .send()
                .await
                .map_err(|error| error.to_string())?
                .bytes()
                .await
                .map_err(|error| error.to_string())?;
            Ok(Arc::from(bytes.to_vec()))
        })
    }
}
