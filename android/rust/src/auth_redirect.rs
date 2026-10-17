//! Captures Spotify's redirect.
//!
//! The Web API grant uses the app's `spotifast://callback` custom scheme:
//! Kotlin hands the code from `onNewIntent` back through
//! `CoreHandle::complete_redirect`, which resolves the in-flight channel here.
//! The playback grant uses Spotify's own client, which only has the loopback
//! redirect registered, so it delegates to the core's loopback listener.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use spotifast_core::auth::{AuthRedirect, AuthRedirectHandler, Grant, PLAYBACK_REDIRECT_PORT};
use spotifast_core::auth_redirect::LoopbackRedirectHandler;
use tokio::sync::{oneshot, watch};

/// The Android redirect handler, shared with the backend and the FFI surface.
#[derive(Default)]
pub struct AndroidAuthRedirect {
    pending: Mutex<Option<oneshot::Sender<String>>>,
}

impl AndroidAuthRedirect {
    /// Kotlin calls this from `onNewIntent` with the `code` query parameter.
    pub fn complete(&self, code: String) {
        if let Some(sender) = self.pending.lock().unwrap().take() {
            let _ = sender.send(code);
        }
    }
}

impl AuthRedirectHandler for AndroidAuthRedirect {
    fn redirect_uri(&self, grant: &Grant) -> String {
        if grant.redirect_port == PLAYBACK_REDIRECT_PORT {
            // Spotify's desktop client only has the loopback redirect registered.
            format!("http://127.0.0.1:{}/login", grant.redirect_port)
        } else {
            "spotifast://callback".to_string()
        }
    }

    fn prepare(&self, grant: &Grant) -> Result<Box<dyn AuthRedirect + Send>> {
        if grant.redirect_port == PLAYBACK_REDIRECT_PORT {
            return LoopbackRedirectHandler.prepare(grant);
        }
        let (sender, receiver) = oneshot::channel();
        *self.pending.lock().unwrap() = Some(sender);
        Ok(Box::new(AndroidRedirect { receiver }))
    }
}

struct AndroidRedirect {
    receiver: oneshot::Receiver<String>,
}

impl AuthRedirect for AndroidRedirect {
    fn wait_for_code(
        self: Box<Self>,
        _expected_state: &str,
        mut cancel: watch::Receiver<bool>,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> {
        Box::pin(async move {
            let mut receiver = self.receiver;
            tokio::select! {
                changed = cancel.changed() => {
                    if changed.is_ok() && *cancel.borrow() {
                        Err(anyhow!("sign-in cancelled"))
                    } else {
                        receiver.await.map_err(|_| anyhow!("sign-in cancelled"))
                    }
                }
                code = &mut receiver => code.map_err(|_| anyhow!("sign-in cancelled")),
            }
        })
    }
}
