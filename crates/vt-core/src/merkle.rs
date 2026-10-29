//! Order-independent Merkle root over manifest entries.

use crate::manifest::{Entry, EntryKind};

const LEAF: u8 = 0x00;
const NODE: u8 = 0x01;

/// Root over `entries`, sorted by `(root tag, rel)` first.
///
/// Leaf: `blake3(0x00 || tag || rel || kind || hash_or_zero || size_le || mode_le)`,
/// variable fields length-prefixed. Node: `blake3(0x01 || left || right)`.
/// An odd node moves up unchanged. An empty list gives `blake3(b"vt-empty")`.
pub fn merkle_root(entries: &[Entry]) -> [u8; 32] {
    if entries.is_empty() {
        return *blake3::hash(b"vt-empty").as_bytes();
    }

    let mut keyed: Vec<(String, &Entry)> = entries.iter().map(|e| (e.root.tag(), e)).collect();
    keyed.sort_by(|a, b| (&a.0, &a.1.rel).cmp(&(&b.0, &b.1.rel)));

    let mut level: Vec<[u8; 32]> = keyed.iter().map(|(tag, e)| leaf(tag, e)).collect();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| match pair {
                [l, r] => {
                    let mut h = blake3::Hasher::new();
                    h.update(&[NODE]).update(l).update(r);
                    *h.finalize().as_bytes()
                }
                [single] => *single,
                _ => unreachable!(),
            })
            .collect();
    }
    level[0]
}

fn leaf(tag: &str, e: &Entry) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&[LEAF]);
    put_str(&mut h, tag);
    put_str(&mut h, &e.rel);
    match &e.kind {
        EntryKind::File => {
            h.update(&[0]);
        }
        EntryKind::Dir => {
            h.update(&[1]);
        }
        EntryKind::Symlink { target } => {
            h.update(&[2]);
            put_str(&mut h, target);
        }
    }
    h.update(&e.hash.unwrap_or([0; 32]));
    h.update(&e.size.to_le_bytes());
    h.update(&e.mode.to_le_bytes());
    *h.finalize().as_bytes()
}

fn put_str(h: &mut blake3::Hasher, s: &str) {
    h.update(&(s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}
