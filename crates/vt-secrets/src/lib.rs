//! Secret storage abstraction.
//!
//! This crate is the only code that touches the OS credential store.
//! [`KeyringStore`] talks to the macOS login Keychain; [`MemStore`] is an
//! in-memory stand-in for tests. Values are raw bytes and are never logged.

use std::path::Path;

use anyhow::Result;
use sha2::{Digest, Sha256};
use vt_core::secret_types::SecretBytes;

mod keyring_store;
mod mem_store;

pub use keyring_store::KeyringStore;
pub use mem_store::MemStore;

/// Keychain service under which Claude Code stores its credentials
/// (generic password, account = login user name).
pub const CLAUDE_KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// A credential store keyed by `(service, account)`.
pub trait SecretStore {
    /// Returns the stored value, or `None` when no such item exists.
    fn get(&self, service: &str, account: &str) -> Result<Option<SecretBytes>>;

    /// Creates the item or replaces its value.
    fn set(&self, service: &str, account: &str, value: &SecretBytes) -> Result<()>;

    /// Removes the item; removing a missing item succeeds.
    fn delete(&self, service: &str, account: &str) -> Result<()>;
}

/// Keychain service name Claude Code uses for the given config directory.
///
/// Without `CLAUDE_CONFIG_DIR` the name is [`CLAUDE_KEYCHAIN_SERVICE`]. With
/// an override, Claude appends `-` and the first 8 lowercase hex characters of
/// the SHA-256 of the directory string exactly as given.
pub fn claude_service_name(claude_config_dir_override: Option<&Path>) -> String {
    match claude_config_dir_override {
        None => CLAUDE_KEYCHAIN_SERVICE.to_owned(),
        Some(dir) => {
            let digest = Sha256::digest(dir.as_os_str().as_encoded_bytes());
            let suffix: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
            format!("{CLAUDE_KEYCHAIN_SERVICE}-{suffix}")
        }
    }
}
