//! In-memory [`SecretStore`] for tests.

use std::collections::BTreeMap;
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use vt_core::secret_types::SecretBytes;

use crate::SecretStore;

type Key = (String, String);

/// Secret store backed by a map; values are zeroized when dropped.
#[derive(Debug, Default)]
pub struct MemStore {
    items: Mutex<BTreeMap<Key, SecretBytes>>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// `(service, account)` of every stored item, sorted.
    pub fn keys(&self) -> Result<Vec<(String, String)>> {
        Ok(self.lock()?.keys().cloned().collect())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<Key, SecretBytes>>> {
        self.items
            .lock()
            .map_err(|_| anyhow!("bellek sır deposu kilidi bozuk"))
    }
}

impl SecretStore for MemStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretBytes>> {
        let items = self.lock()?;
        Ok(items
            .get(&(service.to_owned(), account.to_owned()))
            .cloned())
    }

    fn set(&self, service: &str, account: &str, value: &SecretBytes) -> Result<()> {
        let mut items = self.lock()?;
        items.insert((service.to_owned(), account.to_owned()), value.clone());
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        let mut items = self.lock()?;
        items.remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}
