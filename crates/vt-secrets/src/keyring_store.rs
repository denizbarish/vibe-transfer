//! [`SecretStore`] backed by the OS credential store.
//!
//! On macOS this is a generic password item in the user's login Keychain,
//! which is the item kind Claude Code reads and writes (service, account).
//! Built directly on `keyring-core` with the `apple-native-keyring-store`
//! keychain backend, so no process-wide default store is installed and no
//! other platform backend is linked. Windows support arrives in a later phase.

use anyhow::Result;
use vt_core::secret_types::SecretBytes;

use crate::SecretStore;

/// Secret store using the platform credential store.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use anyhow::{Result, anyhow};
    use apple_native_keyring_store::keychain::{Cred, MacKeychainDomain};
    use keyring_core::{Entry, Error};

    fn entry(service: &str, account: &str) -> Result<Entry> {
        Cred::build(MacKeychainDomain::User, service, account)
            .map_err(|e| anyhow!("Keychain kaydı hazırlanamadı ({service}): {}", describe(&e)))
    }

    pub fn get(service: &str, account: &str) -> Result<Option<Vec<u8>>> {
        match entry(service, account)?.get_secret() {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!(
                "Keychain kaydı okunamadı ({service}): {}",
                describe(&e)
            )),
        }
    }

    pub fn set(service: &str, account: &str, value: &[u8]) -> Result<()> {
        entry(service, account)?
            .set_secret(value)
            .map_err(|e| anyhow!("Keychain kaydı yazılamadı ({service}): {}", describe(&e)))
    }

    pub fn delete(service: &str, account: &str) -> Result<()> {
        match entry(service, account)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow!(
                "Keychain kaydı silinemedi ({service}): {}",
                describe(&e)
            )),
        }
    }

    /// Describes an error without any payload bytes it may carry.
    fn describe(e: &Error) -> String {
        match e {
            Error::PlatformFailure(p) | Error::NoStorageAccess(p) => p.to_string(),
            Error::BadEncoding(_) | Error::BadDataFormat(..) => "veri biçimi geçersiz".to_owned(),
            Error::Invalid(attr, reason) => format!("{attr}: {reason}"),
            _ => "beklenmeyen Keychain hatası".to_owned(),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use anyhow::{Result, bail};

    pub fn get(_service: &str, _account: &str) -> Result<Option<Vec<u8>>> {
        bail!("bu platformda sır deposu henüz desteklenmiyor")
    }

    pub fn set(_service: &str, _account: &str, _value: &[u8]) -> Result<()> {
        bail!("bu platformda sır deposu henüz desteklenmiyor")
    }

    pub fn delete(_service: &str, _account: &str) -> Result<()> {
        bail!("bu platformda sır deposu henüz desteklenmiyor")
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretBytes>> {
        Ok(platform::get(service, account)?.map(SecretBytes::new))
    }

    fn set(&self, service: &str, account: &str, value: &SecretBytes) -> Result<()> {
        platform::set(service, account, value.expose())
    }

    fn delete(&self, service: &str, account: &str) -> Result<()> {
        platform::delete(service, account)
    }
}
