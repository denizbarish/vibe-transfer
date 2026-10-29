//! Single-pass bundle reader with full verification.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use age::DecryptError;
use age::secrecy::SecretString;
use anyhow::{Context, Result};
use serde::Deserialize;
use zeroize::Zeroizing;

use super::{
    BundleError, KIND_END, KIND_MANIFEST, KIND_SECRET, KIND_SEGMENT, MAGIC, RECORD_HEADER,
    ReadBundle, SEGMENT_HEADER, SEGMENT_MAX,
};
use crate::manifest::{EntryKind, FORMAT_VERSION, Manifest};
use crate::merkle::merkle_root;
use crate::secret_types::SecretItem;

/// Upper bounds on frame payloads, checked before allocating.
const MAX_SEGMENT_FRAME: usize = 2 * SEGMENT_MAX;
const MAX_SECRET_FRAME: usize = 16 * 1024 * 1024;
const MAX_MANIFEST_FRAME: usize = 1024 * 1024 * 1024;

/// Decrypts and verifies the bundle at `path`, writing blobs to
/// `staging/blobs/<entry_index>`. Secrets stay in memory. On any error the
/// blobs written by this call are removed and no secrets are returned.
pub fn read_bundle(path: &Path, pass: &SecretString, staging: &Path) -> Result<ReadBundle> {
    let blobs = staging.join("blobs");
    if blobs.exists() && fs::read_dir(&blobs)?.next().is_some() {
        anyhow::bail!("hazırlık dizini boş değil: {}", blobs.display());
    }
    let staging_existed = staging.exists();
    let result = read_inner(path, pass, &blobs);
    match result {
        Ok(read) => Ok(read),
        Err(err) => {
            let mut cleanup = if blobs.exists() {
                fs::remove_dir_all(&blobs)
            } else {
                Ok(())
            };
            if cleanup.is_ok() && !staging_existed && staging.exists() {
                cleanup = fs::remove_dir(staging);
            }
            match cleanup {
                Ok(()) => Err(err),
                Err(e) => Err(err.context(format!(
                    "hazırlık dizini temizlenemedi: {} ({e})",
                    staging.display()
                ))),
            }
        }
    }
}

fn read_inner(path: &Path, pass: &SecretString, blobs: &Path) -> Result<ReadBundle> {
    let identity = super::identity(pass);
    let file = File::open(path).with_context(|| format!("paket açılamadı: {}", path.display()))?;
    let stream = age::Decryptor::new_buffered(BufReader::new(file))
        .and_then(|d| d.decrypt(std::iter::once(&identity as &dyn age::Identity)))
        .map_err(map_age_error)?;
    super::create_private_dir_all(blobs)
        .with_context(|| format!("hazırlık dizini oluşturulamadı: {}", blobs.display()))?;

    let mut st = State {
        input: HashingReader {
            inner: stream,
            hasher: blake3::Hasher::new(),
        },
        identity,
        blobs: blobs.to_path_buf(),
        seq: 0,
        open: None,
        done: HashMap::new(),
        secrets: Vec::new(),
        manifest: None,
    };

    let mut magic = [0u8; 4];
    st.input.exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(BundleError::corrupt("başlık").into());
    }

    loop {
        let before = *st.input.hasher.finalize().as_bytes();
        let mut head = [0u8; 5];
        st.input.exact(&mut head)?;
        let kind = head[0];
        let len = u32::from_be_bytes(head[1..].try_into().unwrap()) as usize;
        match kind {
            KIND_SEGMENT => st.segment(len)?,
            KIND_SECRET => st.secret(len)?,
            KIND_MANIFEST => st.manifest(len)?,
            KIND_END => {
                if len != 32 {
                    return Err(BundleError::corrupt("bitiş çerçevesi").into());
                }
                let mut digest = [0u8; 32];
                st.input.exact(&mut digest)?;
                if digest != before {
                    return Err(BundleError::corrupt("akış özeti").into());
                }
                let mut probe = [0u8; 1];
                if st.input.read_some(&mut probe)? != 0 {
                    return Err(BundleError::corrupt("bitiş sonrası veri").into());
                }
                break;
            }
            other => return Err(BundleError::corrupt(format!("çerçeve türü {other}")).into()),
        }
    }

    st.close_blob();
    let (manifest, root) = st
        .manifest
        .take()
        .ok_or_else(|| BundleError::corrupt("manifest eksik"))?;
    verify(&manifest, &root, &st.done)?;
    Ok(ReadBundle {
        manifest,
        blobs: st.blobs,
        secrets: std::mem::take(&mut st.secrets),
    })
}

/// Checks every blob against the manifest and the stored Merkle root.
fn verify(manifest: &Manifest, root: &[u8; 32], done: &HashMap<u32, [u8; 32]>) -> Result<()> {
    let mut indices = HashSet::new();
    let mut file_indices = HashSet::new();
    for e in &manifest.entries {
        if !indices.insert(e.index) {
            return Err(BundleError::corrupt(format!("öğe {} iki kez", e.index)).into());
        }
        match (&e.kind, e.hash) {
            (EntryKind::File, None) => {
                return Err(BundleError::corrupt(format!("öğe {} hash yok", e.index)).into());
            }
            (EntryKind::File, Some(h)) => {
                if done.get(&e.index) != Some(&h) {
                    return Err(BundleError::corrupt(format!("blob {}", e.index)).into());
                }
                file_indices.insert(e.index);
            }
            (_, Some(_)) => {
                return Err(BundleError::corrupt(format!(
                    "öğe {} dosya değil ama hash var",
                    e.index
                ))
                .into());
            }
            (_, None) => {}
        }
    }
    // Blobs belong to file entries only; a blob for a dir, a symlink or an
    // unknown index is corrupt.
    if let Some(stray) = done.keys().find(|i| !file_indices.contains(i)) {
        return Err(BundleError::corrupt(format!("dosya öğesi olmayan blob {stray}")).into());
    }
    if merkle_root(&manifest.entries) != *root {
        return Err(BundleError::corrupt("merkle kökü").into());
    }
    Ok(())
}

/// Maps header-stage age errors: only a failed key unwrap means a wrong passphrase.
fn map_age_error(e: DecryptError) -> anyhow::Error {
    match e {
        DecryptError::DecryptionFailed | DecryptError::KeyDecryptionFailed => {
            BundleError::WrongPassphrase.into()
        }
        DecryptError::Io(io) if io.kind() != ErrorKind::UnexpectedEof => {
            anyhow::Error::new(io).context("paket okunamadı")
        }
        DecryptError::ExcessiveWork { required, target } => BundleError::corrupt(format!(
            "parola iş faktörü çok yüksek ({required} > {target})"
        ))
        .into(),
        other => BundleError::corrupt(format!("age: {other}")).into(),
    }
}

/// A blob being written; records of one entry arrive back to back.
struct OpenBlob {
    index: u32,
    file: File,
    next_offset: u64,
    hasher: blake3::Hasher,
}

struct State<R> {
    input: HashingReader<R>,
    identity: age::scrypt::Identity,
    blobs: PathBuf,
    seq: u32,
    open: Option<OpenBlob>,
    done: HashMap<u32, [u8; 32]>,
    secrets: Vec<SecretItem>,
    manifest: Option<(Manifest, [u8; 32])>,
}

impl<R: Read> State<R> {
    fn payload(&mut self, len: usize, max: usize, what: &str) -> Result<Vec<u8>> {
        if len > max {
            return Err(BundleError::corrupt(format!("{what} boyutu")).into());
        }
        let mut buf = vec![0u8; len];
        self.input.exact(&mut buf)?;
        Ok(buf)
    }

    fn segment(&mut self, len: usize) -> Result<()> {
        let seq = self.seq;
        let what = format!("segment {seq}");
        if len < SEGMENT_HEADER {
            return Err(BundleError::corrupt(what).into());
        }
        let payload = self.payload(len, MAX_SEGMENT_FRAME, &what)?;
        let (head, zst) = payload.split_at(SEGMENT_HEADER);
        if head[..4] != seq.to_be_bytes() || head[4..] != *blake3::hash(zst).as_bytes() {
            return Err(BundleError::corrupt(what).into());
        }
        let data = zstd::bulk::decompress(zst, SEGMENT_MAX)
            .map_err(|_| BundleError::corrupt(what.clone()))?;

        let mut rest = data.as_slice();
        while !rest.is_empty() {
            if rest.len() < RECORD_HEADER {
                return Err(BundleError::corrupt(what).into());
            }
            let index = u32::from_be_bytes(rest[0..4].try_into().unwrap());
            let offset = u64::from_be_bytes(rest[4..12].try_into().unwrap());
            let n = u32::from_be_bytes(rest[12..16].try_into().unwrap()) as usize;
            rest = &rest[RECORD_HEADER..];
            if rest.len() < n {
                return Err(BundleError::corrupt(what).into());
            }
            let (bytes, tail) = rest.split_at(n);
            self.record(index, offset, bytes, &what)?;
            rest = tail;
        }
        self.seq += 1;
        Ok(())
    }

    fn record(&mut self, index: u32, offset: u64, bytes: &[u8], what: &str) -> Result<()> {
        let continues = matches!(&self.open, Some(b) if b.index == index);
        if !continues {
            self.close_blob();
            if offset != 0 || self.done.contains_key(&index) {
                return Err(BundleError::corrupt(format!("{what}, öğe {index}")).into());
            }
            let path = self.blobs.join(index.to_string());
            let file = super::create_private_file(&path)
                .with_context(|| format!("blob yazılamadı: {}", path.display()))?;
            self.open = Some(OpenBlob {
                index,
                file,
                next_offset: 0,
                hasher: blake3::Hasher::new(),
            });
        }
        let blob = self.open.as_mut().expect("blob is open");
        if offset != blob.next_offset {
            return Err(BundleError::corrupt(format!("{what}, öğe {index}")).into());
        }
        blob.file.write_all(bytes).context("blob yazılamadı")?;
        blob.hasher.update(bytes);
        blob.next_offset += bytes.len() as u64;
        Ok(())
    }

    fn close_blob(&mut self) {
        if let Some(b) = self.open.take() {
            self.done.insert(b.index, *b.hasher.finalize().as_bytes());
        }
    }

    fn secret(&mut self, len: usize) -> Result<()> {
        let sealed = self.payload(len, MAX_SECRET_FRAME, "sır")?;
        let bad = || BundleError::corrupt("sır");
        let mut plain = Zeroizing::new(Vec::with_capacity(sealed.len()));
        age::Decryptor::new_buffered(sealed.as_slice())
            .and_then(|d| d.decrypt(std::iter::once(&self.identity as &dyn age::Identity)))
            .map_err(|_| bad())?
            .read_to_end(&mut plain)
            .map_err(|_| bad())?;
        let item: SecretItem = serde_json::from_slice(&plain).map_err(|_| bad())?;
        self.secrets.push(item);
        Ok(())
    }

    fn manifest(&mut self, len: usize) -> Result<()> {
        if self.manifest.is_some() || len < 32 {
            return Err(BundleError::corrupt("manifest").into());
        }
        let payload = self.payload(len, MAX_MANIFEST_FRAME, "manifest")?;
        let (json, root) = payload.split_at(len - 32);

        #[derive(Deserialize)]
        struct Version {
            format_version: u32,
        }
        let v: Version =
            serde_json::from_slice(json).map_err(|_| BundleError::corrupt("manifest"))?;
        if v.format_version != FORMAT_VERSION {
            return Err(BundleError::UnsupportedVersion(v.format_version).into());
        }
        let manifest: Manifest =
            serde_json::from_slice(json).map_err(|_| BundleError::corrupt("manifest"))?;
        self.manifest = Some((manifest, root.try_into().unwrap()));
        Ok(())
    }
}

/// Hashes every inner stream byte; maps age stream failures to `Corrupt`.
struct HashingReader<R> {
    inner: R,
    hasher: blake3::Hasher,
}

impl<R: Read> HashingReader<R> {
    fn exact(&mut self, buf: &mut [u8]) -> Result<()> {
        self.inner.read_exact(buf).map_err(stream_error)?;
        self.hasher.update(buf);
        Ok(())
    }

    fn read_some(&mut self, buf: &mut [u8]) -> Result<usize> {
        loop {
            match self.inner.read(buf) {
                Ok(n) => {
                    self.hasher.update(&buf[..n]);
                    return Ok(n);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(stream_error(e)),
            }
        }
    }
}

fn stream_error(e: std::io::Error) -> anyhow::Error {
    match e.kind() {
        ErrorKind::InvalidData | ErrorKind::UnexpectedEof => {
            BundleError::corrupt("şifreli akış").into()
        }
        _ => anyhow::Error::new(e).context("paket okunamadı"),
    }
}
