//! Spotifast's portable core, exposed to Kotlin over UniFFI.

uniffi::include_scaffolding!("spotifast");

/// The long-lived core handle: owns the tokio runtime and the backend.
pub struct CoreHandle {
    _runtime: tokio::runtime::Runtime,
}

impl CoreHandle {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("spotifast-android")
            .enable_all()
            .build()
            .expect("unable to start the tokio runtime");
        CoreHandle { _runtime: runtime }
    }

    pub fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

impl Default for CoreHandle {
    fn default() -> Self {
        Self::new()
    }
}
