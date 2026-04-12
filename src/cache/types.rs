//! Core cache types: content hashes, file units, parser version.

use serde::{Deserialize, Serialize};

use crate::parsing::symbols::Symbol;
use crate::parsing::tokenizer::LangFamily;

pub type ParserVersion = u32;

/// BLAKE3 hash of file bytes. Globally unique identifier for file content.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let h = blake3::hash(bytes);
        ContentHash(*h.as_bytes())
    }

    /// Lowercase hex, 64 chars.
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for b in self.0 {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_val(chunk[0])?;
            let lo = hex_val(chunk[1])?;
            out[i] = (hi << 4) | lo;
        }
        Some(ContentHash(out))
    }
}

impl std::fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ContentHash({})", self.to_hex())
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The per-file index payload stored in Layer A.
///
/// Depends only on `(file_bytes, parser_version)`. Contains nothing
/// path-dependent, project-dependent, or repo-dependent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUnit {
    pub parser_version: ParserVersion,
    pub lang: Option<LangFamily>,
    pub symbols: Vec<Symbol>,
    /// Unique FNV-1a word hashes present in this file.
    pub word_hashes: Vec<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_hex_roundtrip() {
        let h = ContentHash::of_bytes(b"hello world");
        let hex = h.to_hex();
        assert_eq!(hex.len(), 64);
        let h2 = ContentHash::from_hex(&hex).unwrap();
        assert_eq!(h, h2);
    }

    #[test]
    fn content_hash_stable() {
        let a = ContentHash::of_bytes(b"quicklsp");
        let b = ContentHash::of_bytes(b"quicklsp");
        assert_eq!(a, b);
        let c = ContentHash::of_bytes(b"different");
        assert_ne!(a, c);
    }
}
