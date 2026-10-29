//! Claude Code project directory naming (`~/.claude/projects/<slug>`).
//!
//! Mirrors Claude Code's JS implementation: it works on UTF-16 code units,
//! so one non-BMP character becomes two dashes.

/// Longest slug Claude Code keeps before appending a hash suffix.
const MAX_SLUG_UNITS: usize = 200;

/// Encodes an absolute project path the way Claude Code names its project
/// directory: every UTF-16 unit outside ASCII `[A-Za-z0-9]` becomes `-`.
/// Results longer than 200 units are cut to 200 and get `-<base36 hash>`.
pub fn claude_slug(path: &str) -> String {
    let slug: String = path
        .encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(b) if b.is_ascii_alphanumeric() => char::from(b),
            _ => '-',
        })
        .collect();
    if slug.len() <= MAX_SLUG_UNITS {
        return slug;
    }
    // The slug is pure ASCII, so byte length equals unit count.
    let hash = i64::from(java_hash(path)).unsigned_abs();
    format!("{}-{}", &slug[..MAX_SLUG_UNITS], base36(hash))
}

/// Java `String.hashCode` over UTF-16 units with wrapping `i32` arithmetic.
pub fn java_hash(s: &str) -> i32 {
    s.encode_utf16().fold(0i32, |h, unit| {
        h.wrapping_mul(31).wrapping_add(i32::from(unit))
    })
}

/// Lowercase base 36, matching JS `Number.prototype.toString(36)`.
fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_owned();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 digits are ASCII")
}
