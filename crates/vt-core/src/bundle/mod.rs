//! Encrypted bundle format: one age file holding segments, secrets and the manifest.
//!
//! Outer layer: age v1, scrypt passphrase recipient, no armor.
//! Inner stream: `b"VTB\x01"`, then frames `[kind: u8][len: u32 BE][payload]`:
//!
//! - kind 1 Segment: `[seq: u32 BE][blake3(zstd_bytes): 32][zstd_bytes]`; the
//!   zstd (level 3) content is a run of records
//!   `[entry_index: u32 BE][offset: u64 BE][len: u32 BE][bytes]`, at most
//!   32 MiB uncompressed per segment.
//! - kind 2 Secret: a JSON [`SecretItem`](crate::secret_types::SecretItem),
//!   age-encrypted again with the same passphrase.
//! - kind 3 Manifest: JSON [`Manifest`](crate::manifest::Manifest) followed by
//!   its 32-byte Merkle root.
//! - kind 4 End: blake3 of every inner stream byte before this frame.

use std::fmt;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;
use anyhow::{Context, Result};

use crate::manifest::Manifest;
use crate::secret_types::SecretItem;

mod passphrase;
mod reader;
mod writer;

pub use passphrase::generate_passphrase;
pub use reader::read_bundle;
pub use writer::BundleWriter;

pub(crate) const MAGIC: &[u8; 4] = b"VTB\x01";
pub(crate) const KIND_SEGMENT: u8 = 1;
pub(crate) const KIND_SECRET: u8 = 2;
pub(crate) const KIND_MANIFEST: u8 = 3;
pub(crate) const KIND_END: u8 = 4;

/// Maximum uncompressed size of one segment.
pub(crate) const SEGMENT_MAX: usize = 32 * 1024 * 1024;
/// Record header inside a segment: index, offset, length.
pub(crate) const RECORD_HEADER: usize = 4 + 8 + 4;
/// Segment payload header: seq and hash.
pub(crate) const SEGMENT_HEADER: usize = 4 + 32;
/// zstd compression level for segments.
pub(crate) const ZSTD_LEVEL: i32 = 3;

/// Result of a finished bundle write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleSummary {
    /// Size of the encrypted bundle file.
    pub bytes: u64,
    pub segments: u32,
    pub merkle_root: [u8; 32],
}

/// A verified bundle. Blobs live in `blobs/<entry_index>`; secrets only in memory.
#[derive(Debug)]
pub struct ReadBundle {
    pub manifest: Manifest,
    pub blobs: PathBuf,
    pub secrets: Vec<SecretItem>,
}

/// Typed bundle failures, carried inside `anyhow::Error`; use `downcast_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleError {
    WrongPassphrase,
    Corrupt { what: String },
    UnsupportedVersion(u32),
}

impl BundleError {
    pub(crate) fn corrupt(what: impl Into<String>) -> Self {
        BundleError::Corrupt { what: what.into() }
    }
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BundleError::WrongPassphrase => write!(f, "paket parolası yanlış"),
            BundleError::Corrupt { what } => write!(f, "paket bozuk: {what}"),
            BundleError::UnsupportedVersion(v) => {
                write!(f, "desteklenmeyen paket sürümü: {v}")
            }
        }
    }
}

impl std::error::Error for BundleError {}

/// Test support only: decrypts the inner stream of `path`, lets `edit` change
/// it and re-encrypts it in place. Holds the whole bundle in memory.
#[doc(hidden)]
pub fn tamper_inner_stream(
    path: &Path,
    pass: &SecretString,
    edit: impl FnOnce(&mut Vec<u8>),
) -> Result<()> {
    let identity = age::scrypt::Identity::new(pass.clone());
    let decryptor = age::Decryptor::new_buffered(BufReader::new(File::open(path)?))?;
    let mut inner = Vec::new();
    decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))?
        .read_to_end(&mut inner)?;
    edit(&mut inner);

    let encryptor = age::Encryptor::with_user_passphrase(pass.clone());
    let mut out = encryptor.wrap_output(BufWriter::new(File::create(path)?))?;
    out.write_all(&inner)?;
    out.finish()?.flush().context("paket yazılamadı")?;
    Ok(())
}
