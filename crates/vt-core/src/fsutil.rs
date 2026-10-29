//! Owner-only file system helpers. Everything vibe-transfer writes under
//! its home holds user data (sessions may contain plaintext tokens).

use std::fs::File;
use std::path::Path;

/// Creates a new file readable only by the owner; fails if it exists.
pub fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)
}

/// Creates `path` and missing parents, each new one owner-only.
pub fn create_private_dir_all(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}
