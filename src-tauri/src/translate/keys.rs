//! API keys in the OS keychain (Secret Service on Linux, Keychain on macOS,
//! Credential Manager on Windows). Keys never touch plain files or logs.

use std::collections::HashMap;
use std::sync::Mutex;

use super::EngineId;

pub const SERVICE: &str = "com.lenslate.app";

pub trait SecretStore: Send + Sync {
    fn get(&self, engine: EngineId) -> Result<Option<String>, String>;
    fn set(&self, engine: EngineId, key: &str) -> Result<(), String>;
    fn delete(&self, engine: EngineId) -> Result<(), String>;
}

/// The OS keychain, one entry per engine under [`SERVICE`].
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self::with_service(SERVICE)
    }

    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    fn entry(&self, engine: EngineId) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, engine.as_str()).map_err(|e| e.to_string())
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, engine: EngineId) -> Result<Option<String>, String> {
        match self.entry(engine)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, engine: EngineId, key: &str) -> Result<(), String> {
        let key = key.trim();
        if key.is_empty() {
            return self.delete(engine);
        }
        self.entry(engine)?
            .set_password(key)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, engine: EngineId) -> Result<(), String> {
        match self.entry(engine)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// In-memory store for tests and as a fallback when no keychain is available.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<EngineId, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, engine: EngineId) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().get(&engine).cloned())
    }

    fn set(&self, engine: EngineId, key: &str) -> Result<(), String> {
        let key = key.trim();
        let mut map = self.0.lock().unwrap();
        if key.is_empty() {
            map.remove(&engine);
        } else {
            map.insert(engine, key.to_string());
        }
        Ok(())
    }

    fn delete(&self, engine: EngineId) -> Result<(), String> {
        self.0.lock().unwrap().remove(&engine);
        Ok(())
    }
}

/// Which engines have a key stored; never returns the keys themselves.
pub fn key_status(store: &dyn SecretStore) -> HashMap<EngineId, bool> {
    EngineId::ALL
        .iter()
        .filter(|id| id.needs_key())
        .map(|&id| (id, matches!(store.get(id), Ok(Some(_)))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(store: &dyn SecretStore, engine: EngineId) {
        store.delete(engine).unwrap();
        assert_eq!(store.get(engine).unwrap(), None);
        store.set(engine, "  secret-123 \n").unwrap();
        assert_eq!(store.get(engine).unwrap().as_deref(), Some("secret-123"));
        store.set(engine, "secret-456").unwrap();
        assert_eq!(store.get(engine).unwrap().as_deref(), Some("secret-456"));
        store.delete(engine).unwrap();
        assert_eq!(store.get(engine).unwrap(), None);
        // Deleting twice is fine.
        store.delete(engine).unwrap();
    }

    #[test]
    fn memory_store_round_trip_and_status() {
        let store = MemoryStore::default();
        round_trip(&store, EngineId::Deepl);
        store.set(EngineId::Claude, "k").unwrap();
        let status = key_status(&store);
        assert_eq!(status.get(&EngineId::Claude), Some(&true));
        assert_eq!(status.get(&EngineId::Deepl), Some(&false));
        assert!(!status.contains_key(&EngineId::GoogleFree));
        store.set(EngineId::Claude, " ").unwrap();
        assert_eq!(store.get(EngineId::Claude).unwrap(), None);
    }

    /// Uses the real OS keychain under a test service name. Skipped (passes
    /// with a note) where no keychain is reachable, e.g. headless Linux CI.
    #[test]
    fn keyring_round_trip() {
        let store = KeyringStore::with_service("com.lenslate.app.test");
        let engine = EngineId::Microsoft;
        if let Err(e) = store.set(engine, "probe").and_then(|_| store.get(engine)) {
            eprintln!("skipping keyring_round_trip: no keychain available ({e})");
            return;
        }
        match store.get(engine) {
            Ok(Some(v)) if v == "probe" => {}
            other => {
                // Some headless backends accept writes but do not persist them.
                eprintln!("skipping keyring_round_trip: keychain does not persist ({other:?})");
                let _ = store.delete(engine);
                return;
            }
        }
        round_trip(&store, engine);
    }
}
