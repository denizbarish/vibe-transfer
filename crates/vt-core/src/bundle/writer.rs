//! Streaming bundle writer.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufWriter, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;
use age::stream::StreamWriter;
use anyhow::{Context, Result, bail};
use zeroize::Zeroizing;

use super::{
    BundleSummary, KIND_END, KIND_MANIFEST, KIND_SECRET, KIND_SEGMENT, MAGIC, RECORD_HEADER,
    SEGMENT_MAX, ZSTD_LEVEL,
};
use crate::manifest::Manifest;
use crate::merkle::merkle_root;
use crate::secret_types::SecretItem;

/// Largest single read from a source file.
const READ_CHUNK: usize = 1024 * 1024;

/// Writes one encrypted bundle. Files go in with [`add_file`](Self::add_file),
/// secrets with [`add_secret`](Self::add_secret), then [`finish`](Self::finish).
pub struct BundleWriter {
    out: HashingWriter<StreamWriter<BufWriter<File>>>,
    pass: SecretString,
    path: PathBuf,
    /// Uncompressed records of the open segment.
    segment: Vec<u8>,
    seq: u32,
    written: HashSet<u32>,
    chunk: Vec<u8>,
}

impl BundleWriter {
    pub fn create(path: &Path, pass: &SecretString) -> Result<Self> {
        let file = File::create(path)
            .with_context(|| format!("paket oluşturulamadı: {}", path.display()))?;
        let stream = age::Encryptor::with_user_passphrase(pass.clone())
            .wrap_output(BufWriter::new(file))
            .context("paket şifrelemesi başlatılamadı")?;
        let mut out = HashingWriter::new(stream);
        out.write_all(MAGIC)?;
        Ok(Self {
            out,
            pass: pass.clone(),
            path: path.to_path_buf(),
            segment: Vec::new(),
            seq: 0,
            written: HashSet::new(),
            chunk: vec![0; READ_CHUNK],
        })
    }

    /// Streams `data` into the bundle as the blob of `entry_index` and returns
    /// the BLAKE3 of exactly the bytes written. An empty source still writes
    /// one zero-length record so the reader creates an empty blob. After an
    /// error the writer holds a partial record and must be discarded.
    pub fn add_file(&mut self, entry_index: u32, data: &mut dyn Read) -> Result<[u8; 32]> {
        if !self.written.insert(entry_index) {
            bail!("öğe pakete iki kez eklenemez: {entry_index}");
        }
        let mut hasher = blake3::Hasher::new();
        let mut offset: u64 = 0;
        loop {
            let mut room = SEGMENT_MAX - self.segment.len();
            if room <= RECORD_HEADER {
                self.flush_segment()?;
                room = SEGMENT_MAX;
            }
            let want = (room - RECORD_HEADER).min(READ_CHUNK);
            let n = read_some(data, &mut self.chunk[..want])
                .with_context(|| format!("öğe okunamadı: {entry_index}"))?;
            if n == 0 && offset > 0 {
                break;
            }
            let bytes = &self.chunk[..n];
            hasher.update(bytes);
            self.segment.extend_from_slice(&entry_index.to_be_bytes());
            self.segment.extend_from_slice(&offset.to_be_bytes());
            self.segment.extend_from_slice(&(n as u32).to_be_bytes());
            self.segment.extend_from_slice(bytes);
            offset += n as u64;
            if n == 0 {
                break;
            }
        }
        Ok(*hasher.finalize().as_bytes())
    }

    /// Adds one secret, encrypted again with the bundle passphrase.
    pub fn add_secret(&mut self, item: &SecretItem) -> Result<()> {
        // Sized so serialization never reallocates and leaves stray copies:
        // each value byte becomes at most "255," in JSON.
        let mut json = Zeroizing::new(Vec::with_capacity(
            item.value.len() * 4 + item.service.len() * 6 + item.account_hint.len() * 6 + 4096,
        ));
        serde_json::to_writer(&mut *json, item).context("sır serileştirilemedi")?;

        let mut sealed = Vec::new();
        let mut enc = age::Encryptor::with_user_passphrase(self.pass.clone())
            .wrap_output(&mut sealed)
            .context("sır şifrelenemedi")?;
        enc.write_all(&json)?;
        enc.finish()?;
        self.out.frame(KIND_SECRET, &[&sealed])?;
        Ok(())
    }

    /// Writes the manifest and end frames and closes the file.
    pub fn finish(mut self, manifest: &Manifest) -> Result<BundleSummary> {
        self.flush_segment()?;
        let root = merkle_root(&manifest.entries);
        let json = serde_json::to_vec(manifest).context("manifest serileştirilemedi")?;
        self.out.frame(KIND_MANIFEST, &[&json, &root])?;
        let digest = *self.out.hasher.finalize().as_bytes();
        self.out.frame(KIND_END, &[&digest])?;

        let file = self
            .out
            .inner
            .finish()?
            .into_inner()
            .map_err(|e| e.into_error())?;
        file.sync_all()?;
        let bytes = std::fs::metadata(&self.path)?.len();
        Ok(BundleSummary {
            bytes,
            segments: self.seq,
            merkle_root: root,
        })
    }

    fn flush_segment(&mut self) -> Result<()> {
        if self.segment.is_empty() {
            return Ok(());
        }
        let zst = zstd::bulk::compress(&self.segment, ZSTD_LEVEL).context("sıkıştırma hatası")?;
        let hash = blake3::hash(&zst);
        self.out.frame(
            KIND_SEGMENT,
            &[&self.seq.to_be_bytes(), hash.as_bytes(), &zst],
        )?;
        self.seq += 1;
        self.segment.clear();
        Ok(())
    }
}

/// One `read`, retried on `Interrupted`.
fn read_some(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match r.read(buf) {
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

/// Hashes every inner stream byte for the end frame.
struct HashingWriter<W> {
    inner: W,
    hasher: blake3::Hasher,
}

impl<W: Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: blake3::Hasher::new(),
        }
    }

    fn frame(&mut self, kind: u8, parts: &[&[u8]]) -> Result<()> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        let len = u32::try_from(len).context("çerçeve çok büyük")?;
        self.write_all(&[kind])?;
        self.write_all(&len.to_be_bytes())?;
        for p in parts {
            self.write_all(p)?;
        }
        Ok(())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
