//! The Android `ProtectedStore` arm, delegating to Kotlin's credential store.

use std::sync::{Arc, Mutex};

use spotifast_core::credentials::{Error, ProtectedStore};

use crate::CredentialStore;

/// Backs the core's credential persistence with the Kotlin `CredentialStore`
/// callback (EncryptedSharedPreferences). The callback is set after the
/// backend is up, so reads and writes before then report the store
/// unavailable.
pub struct AndroidProtectedStore {
    store: Arc<Mutex<Option<Arc<dyn CredentialStore>>>>,
}

impl AndroidProtectedStore {
    /// Returns the store plus a handle the `CoreHandle` shares so
    /// `set_credential_store` can install the callback later.
    pub fn new() -> (Self, Arc<Mutex<Option<Arc<dyn CredentialStore>>>>) {
        let store = Arc::new(Mutex::new(None));
        (Self { store: store.clone() }, store)
    }
}

impl ProtectedStore for AndroidProtectedStore {
    fn read(&mut self, key: &str) -> Result<Option<Vec<u8>>, Error> {
        let store = self.store.lock().unwrap();
        let Some(store) = store.as_ref() else {
            return Err(Error::Unavailable);
        };
        Ok(store.read(key.to_string()))
    }

    fn write(&mut self, key: &str, secret: &[u8]) -> Result<(), Error> {
        let store = self.store.lock().unwrap();
        let Some(store) = store.as_ref() else {
            return Err(Error::Unavailable);
        };
        if store.write(key.to_string(), secret.to_vec()) {
            Ok(())
        } else {
            Err(Error::Unavailable)
        }
    }

    fn delete(&mut self, key: &str) -> Result<(), Error> {
        let store = self.store.lock().unwrap();
        let Some(store) = store.as_ref() else {
            return Err(Error::Unavailable);
        };
        if store.delete(key.to_string()) {
            Ok(())
        } else {
            Err(Error::Unavailable)
        }
    }
}
