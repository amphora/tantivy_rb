/// Stop word checking, ported from Java's SkippingStopFilter.
///
/// Only applies to WORD tokens (not COMPLEX). The stop word list matches
/// Lucene 3.6's ENGLISH_STOP_WORDS_SET used by the Java application.

/// Check if a lowercased token is a stop word.
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
}
