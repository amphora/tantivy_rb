//! Query-side compound tokenizer.
//!
//! Ported from Java's PatentSafeQueryAnalyser pipeline:
//!   Whitespace -> ASCIIFold -> SkippingPunctuationStop -> PreOrPostPunctuationStrip
//!   -> LowerCase -> StopWords -> Stemmer
//!
//! Unlike the index tokenizer, this does NOT do WORD/COMPLEX classification or
//! n-gram expansion. It's a simpler pipeline that cleans up query text to match
//! indexed tokens. Wildcards (`*`, `?`) and quotes (`"`) are preserved through
//! the pipeline for phrase and wildcard query support.

use magnus::Error;
use rust_stemmers::{Algorithm, Stemmer};
use tantivy::tokenizer::*;
use tantivy::Index;

use super::stop_words::is_stop_word;

/// Build and register the compound query tokenizer from Ruby keyword arguments.
///
/// Reads `stemmer:` and `stop_words:` from the kwargs hash. Unlike the index
/// tokenizer, no `leading_strip:`/`trailing_strip:` — the query tokenizer uses
/// its own punctuation rules that preserve wildcards (`*`, `?`) and quotes.
pub fn register_query_tokenizer(
    index: &Index,
    name: &str,
    kwargs: &magnus::RHash,
) -> Result<(), Error> {
    let stop_words_list = crate::tokenizer::default::get_stop_words(kwargs)?;
    let stemmer_algo = crate::tokenizer::default::get_stemmer_algorithm(kwargs)?;

    let tokenizer = CompoundQueryTokenizer::new(stop_words_list, stemmer_algo);
    index.tokenizers().register(name, tokenizer);
    Ok(())
}

/// Query-side compound tokenizer.
///
/// A simpler pipeline than the index tokenizer — no WORD/COMPLEX classification
/// or n-gram expansion. Instead it cleans query text to match what was indexed:
///
/// Pipeline: Whitespace split -> ASCII fold -> skip single-char punctuation ->
///   strip leading/trailing punctuation (preserving wildcards) -> lowercase ->
///   stop word removal -> stem (emitting both stemmed + original at same position)
#[derive(Clone)]
pub struct CompoundQueryTokenizer {
    stop_words: Vec<String>,
    stemmer_algo: Algorithm,
}

impl CompoundQueryTokenizer {
    pub fn new(stop_words: Vec<String>, stemmer_algo: Algorithm) -> Self {
        CompoundQueryTokenizer {
            stop_words,
            stemmer_algo,
        }
    }
}

impl Tokenizer for CompoundQueryTokenizer {
    type TokenStream<'a> = CompoundQueryTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        CompoundQueryTokenStream::new(text, &self.stop_words, self.stemmer_algo)
    }
}

/// Token stream for the compound query tokenizer.
///
/// Like `CompoundIndexTokenStream`, buffers output tokens from each raw token.
/// The buffer holds at most two tokens per raw input (stemmed form + original
/// when they differ).
pub struct CompoundQueryTokenStream<'a> {
    raw_tokens: Vec<&'a str>,
    raw_pos: usize,
    buffer: Vec<Token>,
    buf_pos: usize,
    token: Token,
    stop_words: &'a [String],
    stemmer: Stemmer,
    global_position: usize,
}

impl<'a> CompoundQueryTokenStream<'a> {
    fn new(text: &'a str, stop_words: &'a [String], stemmer_algo: Algorithm) -> Self {
        let raw_tokens: Vec<&str> = text.split_whitespace().collect();
        CompoundQueryTokenStream {
            raw_tokens,
            raw_pos: 0,
            buffer: Vec::new(),
            buf_pos: 0,
            token: Token::default(),
            stop_words,
            stemmer: Stemmer::create(stemmer_algo),
            global_position: 0,
        }
    }

    fn process_next_raw_token(&mut self) -> bool {
        while self.raw_pos < self.raw_tokens.len() {
            let raw = self.raw_tokens[self.raw_pos];
            self.raw_pos += 1;

            // Step 1: ASCII fold
            let folded = super::ascii_fold(raw);

            // Step 2: Skip single-char punctuation (SkippingPunctuationStopFilter).
            // Use chars().count() not len() — len() counts bytes, which would miss
            // multi-byte single characters like accented letters after ASCII folding.
            let mut chars_iter = folded.chars();
            if let Some(first_char) = chars_iter.next() {
                if chars_iter.next().is_none() && !first_char.is_alphanumeric() {
                    continue;
                }
            }

            // Step 3: Strip leading/trailing punctuation (PreOrPostPunctuationStripFilter)
            // Preserves wildcards (*, ?) and quotes (")
            let stripped = strip_query_punctuation(&folded);
            if stripped.is_empty() {
                continue;
            }

            // Step 4: Lowercase
            let lowered = stripped.to_lowercase();

            // Step 5: Stop word filter
            if is_stop_word(&lowered, self.stop_words) {
                continue;
            }

            // Step 6: Stemming — emit stemmed form, plus original if different.
            // Matches Java PatentSafeQueryAnalyser which emits both forms.
            // Both forms are emitted at the SAME position so that
            // build_tokenized_query treats them as synonyms (OR), matching
            // how Lucene handles same-position tokens from the analyzer.
            // The index tokenizer also dual-emits (stemmed + original) so
            // the unstemmed original CAN match in the index, giving BM25
            // a boost for exact-match documents.
            let stemmed = self.stemmer.stem(&lowered).to_string();
            self.global_position += 1;
            let mut tok = Token::default();
            tok.text = stemmed.clone();
            tok.position = self.global_position;
            self.buffer.push(tok);

            // Emit original at same position (synonym) if different from stemmed
            if stemmed != lowered {
                let mut orig_tok = Token::default();
                orig_tok.text = lowered;
                orig_tok.position = self.global_position; // same position = synonym
                self.buffer.push(orig_tok);
            }

            self.buf_pos = 0;
            return true;
        }
        false
    }
}

impl<'a> TokenStream for CompoundQueryTokenStream<'a> {
    fn advance(&mut self) -> bool {
        loop {
            // Drain any buffered tokens first.
            if self.buf_pos < self.buffer.len() {
                let tok = &self.buffer[self.buf_pos];
                self.token.text.clear();
                self.token.text.push_str(&tok.text);
                self.token.position = tok.position;
                self.buf_pos += 1;
                return true;
            }

            // Buffer exhausted — try to fill it from the next raw token.
            self.buffer.clear();
            self.buf_pos = 0;
            if !self.process_next_raw_token() {
                return false;
            }
        }
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

/// Strip leading/trailing punctuation from query tokens.
/// Preserves *, ?, and " (wildcard and phrase characters).
///
/// Ported from Java PreOrPostPunctuationStripFilter.
fn strip_query_punctuation(token: &str) -> &str {
    let trimmed = token.trim_start_matches(is_query_filtered_char);
    trimmed.trim_end_matches(is_query_filtered_char)
}

/// Characters that are filtered from query tokens.
/// Everything except letters, digits, *, ?, and ".
fn is_query_filtered_char(c: char) -> bool {
    if c.is_alphanumeric() {
        return false;
    }
    !matches!(c, '?' | '*' | '"')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_query_punct_tilde() {
        assert_eq!(strip_query_punctuation("~0.45"), "0.45");
    }

    #[test]
    fn test_strip_query_punct_trailing() {
        assert_eq!(strip_query_punctuation("methanol:water;"), "methanol:water");
    }

    #[test]
    fn test_strip_query_punct_both() {
        assert_eq!(strip_query_punctuation("=0.67ml;"), "0.67ml");
    }

    #[test]
    fn test_strip_query_preserves_wildcard() {
        assert_eq!(strip_query_punctuation("print*"), "print*");
        assert_eq!(strip_query_punctuation("print?"), "print?");
    }

    #[test]
    fn test_strip_query_preserves_quote() {
        assert_eq!(strip_query_punctuation("\"hello\""), "\"hello\"");
    }

    #[test]
    fn test_strip_query_empty() {
        assert_eq!(strip_query_punctuation("=;"), "");
    }
}
