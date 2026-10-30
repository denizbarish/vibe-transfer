//! Secret value types shared by `vt-secrets` and the bundle format.
//!
//! [`SecretMeta`] is the value-free summary written into the manifest.
//! [`SecretItem`] carries the value and is only serialized inside an
//! encrypted bundle frame; its `Debug` output never shows the value.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroize;

use crate::manifest::Tool;

/// Secret bytes that are wiped from memory on drop and never printed.
#[derive(Clone)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Gives read access to the raw value.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes(<redacted, {} bytes>)", self.0.len())
    }
}

impl Serialize for SecretBytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretBytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::<u8>::deserialize(d).map(Self)
    }
}

/// Kind of secret store entry. Phase 2 adds the Codex kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    ClaudeKeychain,
}

/// Value-free description of a secret; safe to store in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMeta {
    pub tool: Tool,
    pub kind: SecretKind,
    pub account_uuid: Option<String>,
    pub email: Option<String>,
}

/// A secret with its value. Serialize only into an encrypted frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretItem {
    pub meta: SecretMeta,
    pub service: String,
    pub account_hint: String,
    pub value: SecretBytes,
}
