//! FNV-1a word hashing used for posting-list keys.

/// 32-bit FNV-1a hash of a word. Matches the hash the parser produces for
/// `FileUnit::word_hashes`, so posting-list lookups are O(1).
pub fn word_hash_fnv1a(word: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in word.as_bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}
