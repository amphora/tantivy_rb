//! Stop word checking, ported from Java's SkippingStopFilter.
//!
//! Only applies to WORD tokens (not COMPLEX). The stop word list matches
//! Lucene 3.6's ENGLISH_STOP_WORDS_SET used by the Java application.

/// Check if a lowercased token is a stop word.
///
/// Uses linear scan over the slice, which is fine for the default 33-word
/// English list. If stop word lists grow significantly (hundreds of entries),
/// consider switching to a `HashSet<String>` for O(1) lookups.
pub fn is_stop_word(token: &str, stop_words: &[String]) -> bool {
    stop_words.iter().any(|sw| sw == token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::default::english_stop_words;

    #[test]
    fn test_english_stop_words() {
        let words = english_stop_words();
        assert!(is_stop_word("the", &words));
        assert!(is_stop_word("and", &words));
        assert!(is_stop_word("is", &words));
        assert!(is_stop_word("a", &words));
        assert!(!is_stop_word("hello", &words));
        assert!(!is_stop_word("document", &words));
    }

    #[test]
    fn test_is_stop_word_case_sensitive() {
        let words = english_stop_words();
        // Function operates on exact match — capitalised forms are NOT stop words
        assert!(!is_stop_word("The", &words));
        assert!(!is_stop_word("AND", &words));
        assert!(!is_stop_word("Is", &words));
    }

    #[test]
    fn test_is_stop_word_empty_string() {
        let words = english_stop_words();
        assert!(!is_stop_word("", &words));
    }

    #[test]
    fn test_is_stop_word_partial_match() {
        let words = english_stop_words();
        // "an" and "and" are both stop words
        assert!(is_stop_word("an", &words));
        assert!(is_stop_word("and", &words));
        // "android" is not — no substring matching
        assert!(!is_stop_word("android", &words));
        // "there" IS a stop word, but "thermal" is not
        assert!(is_stop_word("there", &words));
        assert!(!is_stop_word("thermal", &words));
    }

    #[test]
    fn test_is_stop_word_empty_list() {
        let empty: Vec<String> = Vec::new();
        assert!(!is_stop_word("the", &empty));
        assert!(!is_stop_word("hello", &empty));
        assert!(!is_stop_word("", &empty));
    }
}
