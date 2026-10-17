//! Captures Spotify's redirect through the app's custom scheme.
//!
//! The desktop listens on a loopback port; Android registers a
//! `spotifast://callback` intent filter and Kotlin hands the code from
//! `onNewIntent` back through `CoreHandle::complete_redirect`, which
//! resolves the in-flight channel here.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use spotifast_core::auth::{AuthRedirect, AuthRedirectHandler, Grant};
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
    fn redirect_uri(&self, _grant: &Grant) -> String {
        "spotifast://callback".to_string()
    }

    fn prepare(&self, _grant: &Grant) -> Result<Box<dyn AuthRedirect + Send>> {
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
