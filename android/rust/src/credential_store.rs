//! The Android `ProtectedStore` arm, delegating to Kotlin's credential store.

use std::sync::{Arc, Mutex};

use spotifast_core::credentials::{Error, ProtectedStore};

use crate::CredentialStore;

/// Backs the core's credential persistence with the Kotlin `CredentialStore`
/// callback (EncryptedSharedPreferences).
pub struct AndroidProtectedStore {
    store: Arc<Mutex<Option<Arc<dyn CredentialStore>>>>,
}

impl AndroidProtectedStore {
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self {
            store: Arc::new(Mutex::new(Some(store))),
        }
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
