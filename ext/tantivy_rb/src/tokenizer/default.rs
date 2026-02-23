//! Default tokenizer — a standard text analysis pipeline using Tantivy's
//! built-in filters.
//!
//! Pipeline: Whitespace -> ASCII folding -> lowercase -> stop words -> stemmer.
//!
//! This is a simpler alternative to the compound tokenizer, suitable for
//! general-purpose text fields that don't need WORD/COMPLEX classification
//! or n-gram expansion. Also provides shared helpers (`get_stemmer_lang`,
//! `get_stop_words`, `get_strip_chars`) used by other tokenizer types.

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

/// Parse the `stemmer:` option. Defaults to English.
pub fn get_stemmer_lang(kwargs: &RHash) -> Result<Language, Error> {
    let lang = match kwargs.get(Symbol::new("stemmer")) {
        Some(val) => {
            let sym: Symbol = magnus::TryConvert::try_convert(val)?;
            let name = sym.name().map_err(|e| {
                Error::new(magnus::exception::runtime_error(), format!("{}", e))
            })?.to_string();
            match name.to_lowercase().as_str() {
                "english" => Language::English,
                "french" => Language::French,
                "german" => Language::German,
                "spanish" => Language::Spanish,
                "italian" => Language::Italian,
                "portuguese" => Language::Portuguese,
                "dutch" => Language::Dutch,
                "swedish" => Language::Swedish,
                "norwegian" => Language::Norwegian,
                "danish" => Language::Danish,
                "finnish" => Language::Finnish,
                "hungarian" => Language::Hungarian,
                "romanian" => Language::Romanian,
                "russian" => Language::Russian,
                "turkish" => Language::Turkish,
                "arabic" => Language::Arabic,
                _ => {
                    return Err(Error::new(
                        magnus::exception::arg_error(),
                        format!("Unknown stemmer language: {}", name),
                    ))
                }
            }
        }
        None => Language::English,
    };
    Ok(lang)
}

/// Parse the `stemmer:` option into a `rust_stemmers::Algorithm`. Defaults to English.
///
/// Shared by the compound index and query tokenizers which use `rust_stemmers`
/// directly (unlike the default tokenizer which uses Tantivy's built-in `Stemmer`).
///
/// Supported languages: `:english`, `:french`, `:german`, `:spanish`, `:italian`,
/// `:portuguese`, `:dutch`, `:swedish`, `:norwegian`, `:danish`, `:finnish`,
/// `:hungarian`, `:romanian`, `:russian`, `:turkish`.
pub fn get_stemmer_algorithm(kwargs: &RHash) -> Result<Algorithm, Error> {
    let algo = match kwargs.get(Symbol::new("stemmer")) {
        Some(val) => {
            let sym: Symbol = magnus::TryConvert::try_convert(val)?;
            let name = sym.name().map_err(|e| {
                Error::new(magnus::exception::runtime_error(), format!("{}", e))
            })?.to_string();
            match name.to_lowercase().as_str() {
                "english" => Algorithm::English,
                "french" => Algorithm::French,
                "german" => Algorithm::German,
                "spanish" => Algorithm::Spanish,
                "italian" => Algorithm::Italian,
                "portuguese" => Algorithm::Portuguese,
                "dutch" => Algorithm::Dutch,
                "swedish" => Algorithm::Swedish,
                "norwegian" => Algorithm::Norwegian,
                "danish" => Algorithm::Danish,
                "finnish" => Algorithm::Finnish,
                "hungarian" => Algorithm::Hungarian,
                "romanian" => Algorithm::Romanian,
                "russian" => Algorithm::Russian,
                "turkish" => Algorithm::Turkish,
                _ => {
                    return Err(Error::new(
                        magnus::exception::arg_error(),
                        format!("Unknown stemmer language: {}", name),
                    ))
                }
            }
        }
        None => Algorithm::English,
    };
    Ok(algo)
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
                    "english" => Ok(english_stop_words()),
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
        None => Ok(english_stop_words()),
    }
}

/// Lucene 3.6-compatible English stop words (matches the Java SkippingStopFilter).
pub fn english_stop_words() -> Vec<String> {
    vec![
        "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "if", "in", "into", "is",
        "it", "no", "not", "of", "on", "or", "such", "that", "the", "their", "then", "there",
        "these", "they", "this", "to", "was", "will", "with",
    ]
    .into_iter()
    .map(String::from)
    .collect()
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
