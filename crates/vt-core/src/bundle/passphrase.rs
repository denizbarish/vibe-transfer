//! Strong generated bundle passphrases.

use age::secrecy::SecretString;
use rand::RngCore;
use rand::rngs::OsRng;
use zeroize::Zeroizing;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const CHARS: usize = 26;
const GROUP: usize = 4;

/// 26 base32 characters (130 bits) in groups of four joined by `-`,
/// e.g. `ABCD-EFGH-IJKL-MNOP-QRST-UVWX-YZ`.
pub fn generate_passphrase() -> SecretString {
    let mut raw = Zeroizing::new([0u8; CHARS]);
    OsRng.fill_bytes(&mut *raw);

    // Capacity covers every char and separator, so the buffer never reallocates.
    let mut out = String::with_capacity(CHARS + CHARS / GROUP);
    for (i, b) in raw.iter().enumerate() {
        if i > 0 && i % GROUP == 0 {
            out.push('-');
        }
        // 256 is a multiple of 32, so the low five bits are uniform.
        out.push(ALPHABET[usize::from(b & 0x1f)] as char);
    }
    SecretString::from(out)
}
