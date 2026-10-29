//! Reading `~/.claude.json`, which Claude Code rewrites while it runs.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

/// Extra reads after the first one fails to parse.
const RETRIES: u32 = 5;
const RETRY_DELAY: Duration = Duration::from_millis(100);

/// Reads and parses `~/.claude.json`.
///
/// A parse error may mean the file was caught mid-write, so it is read again
/// up to 5 times, 100 ms apart. I/O errors (missing file, no permission)
/// are returned at once.
pub fn read_claude_json_stable(path: &Path) -> Result<Value> {
    let mut retries = 0;
    loop {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        match serde_json::from_slice(&bytes) {
            Ok(value) => return Ok(value),
            Err(_) if retries < RETRIES => {
                retries += 1;
                thread::sleep(RETRY_DELAY);
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "{} geçerli JSON değil ({} okuma denendi)",
                        path.display(),
                        RETRIES + 1
                    )
                });
            }
        }
    }
}
