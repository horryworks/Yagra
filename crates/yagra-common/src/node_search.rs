// SPDX-License-Identifier: AGPL-3.0-only
//! How a node search term is read (ADR-258): split on white space into words, every one of which
//! has to match.
//!
//! One place for the rule, because three surfaces ask it — the Nodes tree, the node picker and
//! MCP `list_nodes` — and the WebUI keeps a copy of the folder half
//! (`web/src/lib/nodeSearch.ts`). The two are pinned to one case table,
//! `web/src/lib/fixtures/searchWords.cases.json`, read by a test on each side.

/// How many words one search may hold. A ninth is refused rather than dropped: a word quietly
/// left out widens the answer, and the operator would read the wider answer as the narrow one.
pub const MAX_SEARCH_WORDS: usize = 8;

/// A term held more words than [`MAX_SEARCH_WORDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooManyWords;

/// The words of `term`, lower-cased, in the order typed, without repeats. An empty or blank term
/// is no words, which means "not searching". White space is anything Unicode calls white space,
/// so the full-width space a Japanese IME types separates words too.
///
/// # Errors
/// [`TooManyWords`] past [`MAX_SEARCH_WORDS`] distinct words.
pub fn search_words(term: &str) -> Result<Vec<String>, TooManyWords> {
    let mut out: Vec<String> = Vec::new();
    for word in term.split(char::is_whitespace).filter(|w| !w.is_empty()) {
        let word = word.to_lowercase();
        if !out.contains(&word) {
            out.push(word);
        }
    }
    if out.len() > MAX_SEARCH_WORDS {
        return Err(TooManyWords);
    }
    Ok(out)
}

/// Whether a word could be part of an IP address — the only words worth comparing with a port's
/// addresses, which sit in a JSON document the query has to unpack per node. A dot or a colon
/// makes it one, and so does a word of digits alone (`10`, `254`).
#[must_use]
pub fn looks_like_address_part(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':')
        && (word.contains('.') || word.contains(':') || word.chars().all(|c| c.is_ascii_digit()))
}

/// Whether every word is found in at least one of `names` (a folder and its ancestors). This is
/// the rule that decides whether a folder is shown whole: "tokyo" alone opens Tokyo, "tokyo core"
/// does not, because "core" names no folder on that path.
#[must_use]
pub fn every_word_in(words: &[String], names: &[&str]) -> bool {
    let lowered: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    !words.is_empty()
        && words
            .iter()
            .all(|w| lowered.iter().any(|n| n.contains(w.as_str())))
}

/// `word` as a `LIKE` pattern body: `\`, `%` and `_` stand for themselves (`sw_01` used to match
/// `sw-01`). The query wraps it in `%…%` and names `\` as the escape.
#[must_use]
pub fn like_escape(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    for c in word.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Cases {
        words: Vec<WordCase>,
        paths: Vec<PathCase>,
        address: Vec<AddressCase>,
    }
    #[derive(serde::Deserialize)]
    struct WordCase {
        term: String,
        /// `null` = refused for too many words.
        words: Option<Vec<String>>,
    }
    #[derive(serde::Deserialize)]
    struct PathCase {
        term: String,
        path: Vec<String>,
        whole: bool,
    }
    #[derive(serde::Deserialize)]
    struct AddressCase {
        word: String,
        address: bool,
    }

    fn cases() -> Cases {
        serde_json::from_str(include_str!(
            "../../../web/src/lib/fixtures/searchWords.cases.json"
        ))
        .expect("the shared case table parses")
    }

    /// The table the WebUI's `nodeSearch.test.ts` runs too. A new case goes there, never here.
    #[test]
    fn search_words_answers_the_case_table_the_webui_shares() {
        let c = cases();
        assert!(c.words.len() >= 10, "the word cases did not load");
        for case in &c.words {
            assert_eq!(
                search_words(&case.term).ok(),
                case.words,
                "term {:?}",
                case.term
            );
        }
    }

    #[test]
    fn every_word_in_answers_the_case_table_the_webui_shares() {
        let c = cases();
        assert!(c.paths.len() >= 6, "the path cases did not load");
        for case in &c.paths {
            let words = search_words(&case.term).expect("a path case has few words");
            let names: Vec<&str> = case.path.iter().map(String::as_str).collect();
            assert_eq!(
                every_word_in(&words, &names),
                case.whole,
                "term {:?} over {:?}",
                case.term,
                case.path
            );
        }
    }

    #[test]
    fn looks_like_address_part_answers_the_case_table() {
        let c = cases();
        assert!(c.address.len() >= 6, "the address cases did not load");
        for case in &c.address {
            assert_eq!(
                looks_like_address_part(&case.word),
                case.address,
                "word {:?}",
                case.word
            );
        }
    }

    #[test]
    fn like_escape_makes_the_wildcards_literal() {
        assert_eq!(like_escape("sw_01"), "sw\\_01");
        assert_eq!(like_escape("50%"), "50\\%");
        assert_eq!(like_escape("a\\b"), "a\\\\b");
        assert_eq!(like_escape("core"), "core");
    }
}
