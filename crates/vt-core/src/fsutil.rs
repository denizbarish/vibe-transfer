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

/// Device and inode of what `path` resolves to (symlinks followed);
/// `None` when it does not exist or the platform has no such identity.
pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Whether `a` and `b` are the same file system object however they are
/// spelled (case variants, firmlinks, symlinks, trailing `/`). Falls back
/// to comparing canonical paths, then the paths themselves.
pub fn same_object(a: &Path, b: &Path) -> bool {
    if let (Some(x), Some(y)) = (file_identity(a), file_identity(b)) {
        return x == y;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}
