//! Typo-Tolerant Resolution via Levenshtein Automaton
//!
//! Stores symbol names and resolves fuzzy queries using bounded edit distance
//! with early termination. Near-zero build cost, fast enough for interactive use.

pub mod deletion_neighborhood;
