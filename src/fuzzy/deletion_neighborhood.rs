//! Fuzzy symbol index using Levenshtein automaton for typo-tolerant resolution.
//!
//! Stores symbol names in a plain Vec. On query, computes bounded edit distance
//! against each symbol with early termination. For k=2 and typical symbol names,
//! non-matches terminate after a few characters, making the scan fast despite
//! being O(n).
//!
//! This replaces the previous deletion-neighborhood approach which precomputed
//! all k=1 and k=2 deletion variants at insert time. That was O(1) at query
//! time but extremely expensive to build (70%+ of dependency indexing time was
//! spent generating and hashing variant strings).

/// Maximum edit distance for fuzzy matching.
const MAX_EDIT_DISTANCE: usize = 2;

/// A fuzzy symbol index using Levenshtein automaton matching.
pub struct DeletionIndex {
    symbols: Vec<String>,
}

impl DeletionIndex {
    pub fn new() -> Self {
        Self {
            symbols: Vec::new(),
        }
    }

    /// Add a symbol to the fuzzy index.
    pub fn insert(&mut self, symbol: &str) {
        self.symbols.push(symbol.to_string());
    }

    /// Resolve a potentially misspelled query to canonical symbols.
    ///
    /// Scans all symbols and returns those within edit distance 2 of the query.
    /// Uses bounded Levenshtein with early termination — non-matches typically
    /// bail after examining only a few characters.
    pub fn resolve(&self, query: &str) -> Vec<&str> {
        let query_chars: Vec<char> = query.chars().collect();
        let query_len = query_chars.len();
        let mut results = Vec::new();

        for symbol in &self.symbols {
            let sym_chars: Vec<char> = symbol.chars().collect();
            let sym_len = sym_chars.len();

            // Length filter: edit distance is at least |len_a - len_b|
            if query_len.abs_diff(sym_len) > MAX_EDIT_DISTANCE {
                continue;
            }

            if bounded_levenshtein(&query_chars, &sym_chars, MAX_EDIT_DISTANCE) {
                if !results.contains(&symbol.as_str()) {
                    results.push(symbol.as_str());
                }
            }
        }

        results
    }

    /// Number of canonical symbols in the index.
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// Clear the entire index.
    pub fn clear(&mut self) {
        self.symbols.clear();
    }
}

impl Default for DeletionIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if two strings are within `max_dist` edit distance using a single-row
/// Levenshtein computation with early termination.
///
/// Returns true if distance <= max_dist, false otherwise.
/// Terminates as soon as the minimum possible distance exceeds max_dist.
#[inline]
fn bounded_levenshtein(a: &[char], b: &[char], max_dist: usize) -> bool {
    let a_len = a.len();
    let b_len = b.len();

    // Ensure a is the shorter string (fewer columns = smaller row buffer)
    if a_len > b_len {
        return bounded_levenshtein(b, a, max_dist);
    }

    // Current row of the edit distance matrix
    let mut row: Vec<usize> = (0..=a_len).collect();

    for (i, &b_char) in b.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        let mut row_min = row[0];

        for (j, &a_char) in a.iter().enumerate() {
            let cost = if a_char == b_char { 0 } else { 1 };
            let val = (row[j + 1] + 1) // deletion
                .min(row[j] + 1) // insertion
                .min(prev + cost); // substitution
            prev = row[j + 1];
            row[j + 1] = val;
            row_min = row_min.min(val);
        }

        // Early termination: if every value in this row exceeds max_dist,
        // no subsequent row can bring it back within range.
        if row_min > max_dist {
            return false;
        }
    }

    row[a_len] <= max_dist
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        let mut idx = DeletionIndex::new();
        idx.insert("socket");

        let results = idx.resolve("socket");
        assert!(results.contains(&"socket"));
    }

    #[test]
    fn typo_resolution_transposition() {
        let mut idx = DeletionIndex::new();
        idx.insert("socket");

        // "sokcet" is a transposition error (edit distance 2)
        let results = idx.resolve("sokcet");
        assert!(
            results.contains(&"socket"),
            "Should resolve transposition typo"
        );
    }

    #[test]
    fn typo_resolution_extra_char() {
        let mut idx = DeletionIndex::new();
        idx.insert("process");

        // "processs" has an extra 's' — edit distance 1
        let results = idx.resolve("processs");
        assert!(
            results.contains(&"process"),
            "Should resolve extra-char typo"
        );
    }

    #[test]
    fn typo_substitution() {
        let mut idx = DeletionIndex::new();
        idx.insert("HashMap");

        let results = idx.resolve("HashMpa");
        assert!(results.contains(&"HashMap"), "Should resolve substitution");
    }

    #[test]
    fn no_match_beyond_distance() {
        let mut idx = DeletionIndex::new();
        idx.insert("socket");

        // "abcdef" is far from "socket"
        let results = idx.resolve("abcdef");
        assert!(results.is_empty());
    }

    #[test]
    fn length_filter() {
        let mut idx = DeletionIndex::new();
        idx.insert("ab");

        // "abcdef" differs by 4 chars in length — skip without computing
        let results = idx.resolve("abcdef");
        assert!(results.is_empty());
    }

    #[test]
    fn quality_against_realistic_symbol_set() {
        let mut idx = DeletionIndex::new();
        let symbols = &[
            "HashMap", "HashSet", "BTreeMap", "BTreeSet", "Vec", "String",
            "process_data", "process_event", "process_request",
            "handle_connection", "handle_error", "handle_request",
            "serialize", "deserialize", "Deserializer",
            "Config", "Context", "Connection", "Controller",
            "read_file", "read_line", "write_file", "write_line",
            "TokenKind", "Token", "Tokenizer",
            "SymbolKind", "SymbolLocation", "Symbol",
            "Workspace", "DependencyIndex",
        ];
        for s in symbols {
            idx.insert(s);
        }

        // Transpositions (edit distance 2)
        let r = idx.resolve("HashMpa");
        assert!(r.contains(&"HashMap"), "transposition in HashMap");
        assert!(!r.contains(&"HashSet"), "HashSet is dist 3 from HashMpa");

        // Single substitution (edit distance 1)
        let r = idx.resolve("Comfig");
        assert!(r.contains(&"Config"), "substitution: Comfig → Config");
        assert!(!r.contains(&"Context"), "Context is dist 3 from Comfig");

        // Extra character (edit distance 1)
        let r = idx.resolve("Stringg");
        assert!(r.contains(&"String"), "extra char: Stringg → String");

        // Missing character (edit distance 1)
        let r = idx.resolve("Tken");
        assert!(r.contains(&"Token"), "missing char: Tken → Token");
        assert!(!r.contains(&"TokenKind"), "TokenKind is too far from Tken");

        // Prefix typo (edit distance 1)
        let r = idx.resolve("derialize");
        assert!(r.contains(&"serialize"), "missing char: derialize → serialize");
        assert!(r.contains(&"deserialize"), "missing char at different pos");

        // No false positives for distant strings
        let r = idx.resolve("foobar");
        assert!(r.is_empty(), "foobar should match nothing");

        // Exact match still works amid similar names
        let r = idx.resolve("handle_error");
        assert!(r.contains(&"handle_error"), "exact match");
        assert!(!r.contains(&"handle_connection"), "too distant");

        // Short symbols: more sensitive to typos
        let r = idx.resolve("Vex");
        assert!(r.contains(&"Vec"), "substitution in short symbol");

        // Verify we don't return the whole index
        let r = idx.resolve("process_daata");
        assert!(r.contains(&"process_data"), "extra char in middle");
        assert_eq!(r.len(), 1, "should only match process_data, got: {:?}", r);
    }

    #[test]
    fn bounded_levenshtein_basic() {
        let a: Vec<char> = "kitten".chars().collect();
        let b: Vec<char> = "sitting".chars().collect();
        // kitten → sitting = distance 3
        assert!(!bounded_levenshtein(&a, &b, 2));

        let c: Vec<char> = "socket".chars().collect();
        let d: Vec<char> = "sokcet".chars().collect();
        // transposition = distance 2
        assert!(bounded_levenshtein(&c, &d, 2));
    }
}
