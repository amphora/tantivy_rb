/// Query-side compound tokenizer.
///
/// Ported from Java's PatentSafeQueryAnalyser pipeline:
///   Whitespace → ASCIIFold → SkippingPunctuationStop → PreOrPostPunctuationStrip
///   → LowerCase → StopWords → Stemmer
///
/// Unlike the index tokenizer, this does NOT do WORD/COMPLEX classification or n-gram
/// expansion. It's a simpler pipeline that cleans up query text to match indexed tokens.

use magnus::{Error, RHash, Symbol};
use rust_stemmers::{Algorithm, Stemmer};
use tantivy::tokenizer::*;
use tantivy::Index;

use super::stop_words::is_stop_word;

pub fn register_query_tokenizer(
    index: &Index,
    name: &str,
    kwargs: &RHash,
) -> Result<(), Error> {
    let stop_words_list = crate::tokenizer::default::get_stop_words(kwargs)?;

    let stemmer_algo = match kwargs.get(Symbol::new("stemmer")) {
        Some(val) => {
            let sym: Symbol = magnus::TryConvert::try_convert(val)?;
            let name = sym
                .name()
                .map_err(|e| Error::new(magnus::exception::runtime_error(), format!("{}", e)))?
                .to_string();
            match name.to_lowercase().as_str() {
                "english" => Algorithm::English,
                "french" => Algorithm::French,
                "german" => Algorithm::German,
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

    let tokenizer = CompoundQueryTokenizer::new(stop_words_list, stemmer_algo);
    index.tokenizers().register(name, tokenizer);
    Ok(())
}

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

            // Step 2: Skip single-char punctuation (SkippingPunctuationStopFilter)
            if folded.len() == 1 {
                let c = folded.chars().next().unwrap();
                if !c.is_alphanumeric() {
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
        if self.buf_pos < self.buffer.len() {
            let tok = &self.buffer[self.buf_pos];
            self.token.text.clear();
            self.token.text.push_str(&tok.text);
            self.token.position = tok.position;
            self.buf_pos += 1;
            return true;
        }
        self.buffer.clear();
        self.buf_pos = 0;

        if self.process_next_raw_token() {
            if self.buf_pos < self.buffer.len() {
                let tok = &self.buffer[self.buf_pos];
                self.token.text.clear();
                self.token.text.push_str(&tok.text);
                self.token.position = tok.position;
                self.buf_pos += 1;
                return true;
            }
        }
        false
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
fn strip_query_punctuation(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    let mut start = 0;
    let mut end = chars.len();

    while start < end && is_query_filtered_char(chars[start]) {
        start += 1;
    }
    while end > start && is_query_filtered_char(chars[end - 1]) {
        end -= 1;
    }

    if end <= start {
        return String::new();
    }

    chars[start..end].iter().collect()
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
