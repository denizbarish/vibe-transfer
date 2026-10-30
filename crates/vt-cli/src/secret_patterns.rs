//! Detects plaintext secrets in files without ever returning the value.
//!
//! Patterns: GitHub tokens (`ghp_`, `github_pat_`), Slack tokens
//! (`xoxb-`, `xoxp-`), Anthropic keys (`sk-ant-`) and PEM private key
//! headers (`-----BEGIN ... PRIVATE KEY-----`).

use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::path::Path;

/// A token: a prefix followed by at least `min` token characters.
struct TokenRule {
    prefix: &'static [u8],
    min: usize,
    /// Allowed besides ASCII letters and digits.
    extra: &'static [u8],
}

const TOKENS: &[TokenRule] = &[
    TokenRule {
        prefix: b"ghp_",
        min: 20,
        extra: b"",
    },
    TokenRule {
        prefix: b"github_pat_",
        min: 20,
        extra: b"_",
    },
    TokenRule {
        prefix: b"xoxb-",
        min: 10,
        extra: b"-",
    },
    TokenRule {
        prefix: b"xoxp-",
        min: 10,
        extra: b"-",
    },
    TokenRule {
        prefix: b"sk-ant-",
        min: 10,
        extra: b"_-",
    },
];

const PEM_START: &[u8] = b"-----BEGIN ";
const PEM_END: &[u8] = b"PRIVATE KEY-----";
/// Longest key type label between the PEM markers (e.g. `ENCRYPTED `).
const PEM_GAP: usize = 64;

const CHUNK: usize = 256 * 1024;
/// Bytes carried over between chunks; longer than any match.
const OVERLAP: usize = 128;

/// Whether `buf` holds a secret pattern.
pub fn has_secret_pattern(buf: &[u8]) -> bool {
    TOKENS.iter().any(|rule| has_token(buf, rule)) || has_pem_key(buf)
}

/// Whether the file at `path` holds a secret pattern, read as a stream.
pub fn file_has_secret_pattern(path: &Path) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; CHUNK + OVERLAP];
    let mut kept = 0;
    loop {
        let n = match file.read(&mut buf[kept..kept + CHUNK]) {
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            return Ok(false);
        }
        let end = kept + n;
        if has_secret_pattern(&buf[..end]) {
            return Ok(true);
        }
        kept = end.min(OVERLAP);
        buf.copy_within(end - kept..end, 0);
    }
}

fn positions<'a>(buf: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    buf.windows(needle.len())
        .enumerate()
        .filter(move |(_, w)| *w == needle)
        .map(|(i, _)| i)
}

fn has_token(buf: &[u8], rule: &TokenRule) -> bool {
    positions(buf, rule.prefix).any(|at| {
        let rest = &buf[at + rule.prefix.len()..];
        let run = rest
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || rule.extra.contains(b))
            .count();
        run >= rule.min
    })
}

fn has_pem_key(buf: &[u8]) -> bool {
    positions(buf, PEM_START).any(|at| {
        let rest = &buf[at + PEM_START.len()..];
        let window = &rest[..rest.len().min(PEM_GAP + PEM_END.len())];
        let line = window.split(|b| *b == b'\n').next().unwrap_or_default();
        positions(line, PEM_END).next().is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Token-shaped values are assembled at run time so the source never
    /// holds one.
    fn token(prefix: &str, len: usize) -> Vec<u8> {
        format!("x={prefix}{}\n", "a1".repeat(len / 2)).into_bytes()
    }

    #[test]
    fn detects_each_pattern() {
        assert!(has_secret_pattern(&token(&["gh", "p_"].concat(), 24)));
        assert!(has_secret_pattern(&token(
            &["github", "_pat_"].concat(),
            24
        )));
        assert!(has_secret_pattern(&token(&["xo", "xb-"].concat(), 12)));
        assert!(has_secret_pattern(&token(&["xo", "xp-"].concat(), 12)));
        assert!(has_secret_pattern(&token(&["sk-", "ant-"].concat(), 12)));
        let pem = ["-----BEGIN ", "RSA PRIVATE KEY-----\nabc"].concat();
        assert!(has_secret_pattern(pem.as_bytes()));
    }

    #[test]
    fn ignores_short_or_unrelated_text() {
        assert!(!has_secret_pattern(&token(&["gh", "p_"].concat(), 8)));
        assert!(!has_secret_pattern(b"the ghp_ prefix alone"));
        let cert = ["-----BEGIN ", "CERTIFICATE-----\n"].concat();
        assert!(!has_secret_pattern(cert.as_bytes()));
        let split = ["-----BEGIN ", "RSA\nPRIVATE KEY-----"].concat();
        assert!(!has_secret_pattern(split.as_bytes()));
    }

    #[test]
    fn finds_pattern_across_chunk_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let mut data = vec![b'.'; CHUNK - 10];
        data.extend_from_slice(&token(&["sk-", "ant-"].concat(), 20));
        std::fs::write(&path, &data).unwrap();
        assert!(file_has_secret_pattern(&path).unwrap());

        std::fs::write(&path, vec![b'.'; CHUNK * 2 + 5]).unwrap();
        assert!(!file_has_secret_pattern(&path).unwrap());
    }
}
