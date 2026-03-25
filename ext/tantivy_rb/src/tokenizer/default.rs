//! Default tokenizer — a standard text analysis pipeline using Tantivy's
//! built-in filters.
//!
//! Pipeline: Whitespace -> ASCII folding -> lowercase -> stop words -> stemmer.
//!
//! This is a simpler alternative to the compound tokenizer, suitable for
//! general-purpose text fields that don't need WORD/COMPLEX classification
//! or n-gram expansion. Also provides shared helpers (`get_stemmer_lang`,
//! `get_stop_words`, `get_strip_chars`) used by other tokenizer types.

use std::sync::LazyLock;

use magnus::{Error, RHash, Symbol};
use rust_stemmers::Algorithm;
use tantivy::tokenizer::*;
use tantivy::Index;

/// Register a `:default` tokenizer: Whitespace → AsciiFolding → LowerCase → StopWords → Stemmer
pub fn register(index: &Index, name: &str, kwargs: &RHash) -> Result<(), Error> {
    let stemmer_lang = get_stemmer_lang(kwargs)?;
    let stop_words = get_stop_words(kwargs)?;

    let tokenizer = TextAnalyzer::builder(WhitespaceTokenizer::default())
        .filter(AsciiFoldingFilter)
        .filter(LowerCaser)
        .filter(StopWordFilter::remove(stop_words))
        .filter(Stemmer::new(stemmer_lang))
        .build();

    index.tokenizers().register(name, tokenizer);
    Ok(())
}

/// Supported stemmer languages: `:english`, `:french`, `:german`, `:spanish`,
/// `:italian`, `:portuguese`, `:dutch`, `:swedish`, `:norwegian`, `:danish`,
/// `:finnish`, `:hungarian`, `:romanian`, `:russian`, `:turkish`, `:arabic`.
///
/// Single lookup table maps language name → (tantivy Language, rust_stemmers Algorithm).
/// Used by both `get_stemmer_lang` and `get_stemmer_algorithm` to keep language
/// support in sync without duplication.
const LANGUAGE_TABLE: &[(&str, Language, Algorithm)] = &[
    ("english", Language::English, Algorithm::English),
    ("french", Language::French, Algorithm::French),
    ("german", Language::German, Algorithm::German),
    ("spanish", Language::Spanish, Algorithm::Spanish),
    ("italian", Language::Italian, Algorithm::Italian),
    ("portuguese", Language::Portuguese, Algorithm::Portuguese),
    ("dutch", Language::Dutch, Algorithm::Dutch),
    ("swedish", Language::Swedish, Algorithm::Swedish),
    ("norwegian", Language::Norwegian, Algorithm::Norwegian),
    ("danish", Language::Danish, Algorithm::Danish),
    ("finnish", Language::Finnish, Algorithm::Finnish),
    ("hungarian", Language::Hungarian, Algorithm::Hungarian),
    ("romanian", Language::Romanian, Algorithm::Romanian),
    ("russian", Language::Russian, Algorithm::Russian),
    ("turkish", Language::Turkish, Algorithm::Turkish),
    ("arabic", Language::Arabic, Algorithm::Arabic),
];

/// Parse a Ruby `:stemmer` symbol into a `(Language, Algorithm)` pair.
/// Returns English defaults if no `:stemmer` key is present.
fn parse_stemmer_option(kwargs: &RHash) -> Result<(Language, Algorithm), Error> {
    match kwargs.get(Symbol::new("stemmer")) {
        Some(val) => {
            let sym: Symbol = magnus::TryConvert::try_convert(val)?;
            let name = sym.name().map_err(|e| {
                Error::new(magnus::exception::runtime_error(), format!("{}", e))
            })?.to_string();
            let lower = name.to_lowercase();
            LANGUAGE_TABLE
                .iter()
                .find(|(n, _, _)| *n == lower)
                .map(|(_, lang, algo)| (*lang, *algo))
                .ok_or_else(|| {
                    Error::new(
                        magnus::exception::arg_error(),
                        format!("Unknown stemmer language: {}", name),
                    )
                })
        }
        None => Ok((Language::English, Algorithm::English)),
    }
}

/// Parse the `stemmer:` option into a tantivy `Language`. Defaults to English.
pub fn get_stemmer_lang(kwargs: &RHash) -> Result<Language, Error> {
    parse_stemmer_option(kwargs).map(|(lang, _)| lang)
}

/// Parse the `stemmer:` option into a `rust_stemmers::Algorithm`. Defaults to English.
///
/// Shared by the compound index and query tokenizers which use `rust_stemmers`
/// directly (unlike the default tokenizer which uses Tantivy's built-in `Stemmer`).
pub fn get_stemmer_algorithm(kwargs: &RHash) -> Result<Algorithm, Error> {
    parse_stemmer_option(kwargs).map(|(_, algo)| algo)
}

/// Parse the `stop_words:` option. Accepts a language symbol or array of strings.
/// Defaults to English stop words.
pub fn get_stop_words(kwargs: &RHash) -> Result<Vec<String>, Error> {
    match kwargs.get(Symbol::new("stop_words")) {
        Some(val) => {
            // Try as symbol first (language name)
            if let Ok(sym) = <Symbol as magnus::TryConvert>::try_convert(val) {
                let name = sym.name().map_err(|e| {
                    Error::new(magnus::exception::runtime_error(), format!("{}", e))
                })?.to_string();
                match name.to_lowercase().as_str() {
                    "english" => Ok(english_stop_words().to_vec()),
                    _ => Err(Error::new(
                        magnus::exception::arg_error(),
                        format!("Unknown stop words language: {}", name),
                    )),
                }
            } else {
                // Try as array of strings
                let arr: magnus::RArray = magnus::TryConvert::try_convert(val)?;
                let mut words = Vec::new();
                for item in arr.into_iter() {
                    let s: String = magnus::TryConvert::try_convert(item)?;
                    words.push(s);
                }
                Ok(words)
            }
        }
        None => Ok(english_stop_words().to_vec()),
    }
}

/// Lucene 3.6-compatible English stop words (matches the Java SkippingStopFilter).
///
/// Backed by a `LazyLock` static — the list is allocated once on first access.
/// Returns a borrowed slice; callers that need ownership should call `.to_vec()`.
static ENGLISH_STOP_WORDS: LazyLock<Vec<String>> = LazyLock::new(|| {
    vec![
        "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is",
        "it", "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there",
        "these", "they", "this", "to", "was", "will", "with",
    ]
    .into_iter()
    .map(String::from)
    .collect()
});

pub fn english_stop_words() -> &'static [String] {
    &ENGLISH_STOP_WORDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_english_stop_words_count() {
        let words = english_stop_words();
        assert_eq!(words.len(), 33, "Expected 33 English stop words, got {}", words.len());
    }

    #[test]
    fn test_english_stop_words_known_entries() {
        let words = english_stop_words();
        for expected in &["a", "the", "and", "is", "with", "not", "or", "for", "to"] {
            assert!(
                words.iter().any(|w| w == expected),
                "Expected stop word '{}' not found",
                expected
            );
        }
    }

    #[test]
    fn test_english_stop_words_no_duplicates() {
        let words = english_stop_words();
        let mut seen = std::collections::HashSet::new();
        for word in words {
            assert!(seen.insert(word), "Duplicate stop word: '{}'", word);
        }
    }
}

/// Parse the `leading_strip:` or `trailing_strip:` char set from kwargs.
pub fn get_strip_chars(kwargs: &RHash, key: &str) -> Result<Vec<char>, Error> {
    match kwargs.get(Symbol::new(key)) {
        Some(val) => {
            let s: String = magnus::TryConvert::try_convert(val)?;
            Ok(s.chars().collect())
        }
        None => Ok(Vec::new()),
    }
}
