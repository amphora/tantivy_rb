pub mod classifier;
pub mod expander;
pub mod query;
pub mod stop_words;

use classifier::{classify_token, strip_punctuation, TokenKind};
use expander::expand_complex_token;
use stop_words::is_stop_word;

use magnus::{Error, RHash, Symbol};
use rust_stemmers::{Algorithm, Stemmer};
use tantivy::tokenizer::*;
use tantivy::Index;

/// Register a compound tokenizer on the index.
///
/// Options:
///   mode: :index or :query
///   stemmer: language symbol (default :english)
///   stop_words: :english or array of strings
///   leading_strip: string of chars to strip from token start
///   trailing_strip: string of chars to strip from token end
pub fn register(index: &Index, name: &str, kwargs: &RHash) -> Result<(), Error> {
    let mode_sym: Symbol = kwargs
        .get(Symbol::new("mode"))
        .ok_or_else(|| {
            Error::new(
                magnus::exception::arg_error(),
                "compound tokenizer requires mode: :index or :query",
            )
        })
        .and_then(|v| magnus::TryConvert::try_convert(v))?;

    let mode_name = mode_sym
        .name()
        .map_err(|e| Error::new(magnus::exception::runtime_error(), format!("{}", e)))?
        .to_string();

    match mode_name.as_str() {
        "index" => register_index_tokenizer(index, name, kwargs),
        "query" => query::register_query_tokenizer(index, name, kwargs),
        _ => Err(Error::new(
            magnus::exception::arg_error(),
            format!(
                "Unknown compound mode '{}'. Valid modes: index, query",
                mode_name
            ),
        )),
    }
}

fn register_index_tokenizer(index: &Index, name: &str, kwargs: &RHash) -> Result<(), Error> {
    let leading_strip = super::default::get_strip_chars(kwargs, "leading_strip")?;
    let trailing_strip = super::default::get_strip_chars(kwargs, "trailing_strip")?;
    let stop_words_list = super::default::get_stop_words(kwargs)?;

    let stemmer_lang = match kwargs.get(Symbol::new("stemmer")) {
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

    let tokenizer = CompoundIndexTokenizer::new(
        leading_strip,
        trailing_strip,
        stop_words_list,
        stemmer_lang,
    );

    index.tokenizers().register(name, tokenizer);
    Ok(())
}

/// The monolithic compound index tokenizer.
///
/// Pipeline:
///   Whitespace → BlockTokenParse (strip punct + classify) → ASCIIFold → LowerCase
///   → ComplexExpand → SkippingStop → SkippingStemmer
///
/// Implemented as a single Tantivy Tokenizer because the pipeline needs to pass
/// WORD/COMPLEX metadata between stages, which Tantivy's TokenFilter trait doesn't support.
#[derive(Clone)]
pub struct CompoundIndexTokenizer {
    leading_strip: Vec<char>,
    trailing_strip: Vec<char>,
    stop_words: Vec<String>,
    stemmer_algo: Algorithm,
}

impl CompoundIndexTokenizer {
    pub fn new(
        leading_strip: Vec<char>,
        trailing_strip: Vec<char>,
        stop_words: Vec<String>,
        stemmer_algo: Algorithm,
    ) -> Self {
        CompoundIndexTokenizer {
            leading_strip,
            trailing_strip,
            stop_words,
            stemmer_algo,
        }
    }
}

impl Tokenizer for CompoundIndexTokenizer {
    type TokenStream<'a> = CompoundIndexTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        CompoundIndexTokenStream::new(
            text,
            &self.leading_strip,
            &self.trailing_strip,
            &self.stop_words,
            self.stemmer_algo,
        )
    }
}

pub struct CompoundIndexTokenStream<'a> {
    /// The whitespace-split tokens from the original text.
    raw_tokens: Vec<&'a str>,
    /// Current position in raw_tokens.
    raw_pos: usize,
    /// Buffered output tokens (expanded from a single raw token).
    buffer: Vec<Token>,
    /// Current position in buffer.
    buf_pos: usize,
    /// The current output token.
    token: Token,
    /// Config
    leading_strip: &'a [char],
    trailing_strip: &'a [char],
    stop_words: &'a [String],
    stemmer: Stemmer,
    /// Global position counter for the output stream.
    global_position: usize,
}

impl<'a> CompoundIndexTokenStream<'a> {
    fn new(
        text: &'a str,
        leading_strip: &'a [char],
        trailing_strip: &'a [char],
        stop_words: &'a [String],
        stemmer_algo: Algorithm,
    ) -> Self {
        let raw_tokens: Vec<&str> = text.split_whitespace().collect();
        CompoundIndexTokenStream {
            raw_tokens,
            raw_pos: 0,
            buffer: Vec::new(),
            buf_pos: 0,
            token: Token::default(),
            leading_strip,
            trailing_strip,
            stop_words,
            stemmer: Stemmer::create(stemmer_algo),
            global_position: 0,
        }
    }

    /// Process the next raw token into the buffer.
    fn process_next_raw_token(&mut self) -> bool {
        while self.raw_pos < self.raw_tokens.len() {
            let raw = self.raw_tokens[self.raw_pos];
            self.raw_pos += 1;

            // Step 1: Strip leading/trailing punctuation (BlockTokenParsingFilter)
            let stripped = strip_punctuation(raw, self.leading_strip, self.trailing_strip);
            if stripped.is_empty() {
                continue;
            }

            // Step 2: Classify as WORD or COMPLEX
            let kind = classify_token(&stripped);
            if kind == TokenKind::Skip {
                continue;
            }

            // Step 3: ASCII fold
            let folded = ascii_fold(&stripped);

            // Step 4: Lowercase
            let lowered = folded.to_lowercase();

            match kind {
                TokenKind::Word => {
                    // Step 5: Stop word check (only for WORD tokens)
                    if is_stop_word(&lowered, self.stop_words) {
                        continue;
                    }
                    // Step 6: Stemming (only for WORD tokens)
                    // Emit the stemmed form at the next position, plus the original
                    // lowercased form at the SAME position when different.
                    // Same-position tokens act as synonyms in Tantivy — the
                    // QueryParser won't create a phrase query across them.
                    // This matches the Java FullIndexingAnalyser behaviour and allows
                    // BM25 to rank exact-match documents higher: a doc containing
                    // "Requirments" matches both "requir" (stemmed) and "requirments"
                    // (original), scoring higher than docs with only "Requirements"
                    // which only match "requir".
                    let stemmed = self.stemmer.stem(&lowered).to_string();
                    self.global_position += 1;
                    let mut tok = Token::default();
                    tok.text = stemmed.clone();
                    tok.position = self.global_position;
                    self.buffer.push(tok);

                    if stemmed != lowered {
                        let mut orig_tok = Token::default();
                        orig_tok.text = lowered.clone();
                        orig_tok.position = self.global_position; // same position = synonym
                        self.buffer.push(orig_tok);
                    }
                }
                TokenKind::Complex => {
                    // Step 5: N-gram expansion (only for COMPLEX tokens)
                    let expanded = expand_complex_token(&lowered);
                    self.global_position += 1;
                    for sub in expanded.into_iter() {
                        let mut tok = Token::default();
                        tok.text = sub;
                        tok.position = self.global_position; // all at same position
                        self.buffer.push(tok);
                    }
                }
                TokenKind::Skip => unreachable!(),
            }

            self.buf_pos = 0;
            return true;
        }
        false
    }
}

impl<'a> TokenStream for CompoundIndexTokenStream<'a> {
    fn advance(&mut self) -> bool {
        // Return buffered tokens first
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

        // Process next raw token
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

/// Basic ASCII folding: replace common accented characters with ASCII equivalents.
fn ascii_fold(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{00C0}'..='\u{00C5}' => result.push('A'),
            '\u{00C6}' => result.push_str("AE"),
            '\u{00C7}' => result.push('C'),
            '\u{00C8}'..='\u{00CB}' => result.push('E'),
            '\u{00CC}'..='\u{00CF}' => result.push('I'),
            '\u{00D0}' => result.push('D'),
            '\u{00D1}' => result.push('N'),
            '\u{00D2}'..='\u{00D6}' => result.push('O'),
            '\u{00D8}' => result.push('O'),
            '\u{00D9}'..='\u{00DC}' => result.push('U'),
            '\u{00DD}' => result.push('Y'),
            '\u{00E0}'..='\u{00E5}' => result.push('a'),
            '\u{00E6}' => result.push_str("ae"),
            '\u{00E7}' => result.push('c'),
            '\u{00E8}'..='\u{00EB}' => result.push('e'),
            '\u{00EC}'..='\u{00EF}' => result.push('i'),
            '\u{00F0}' => result.push('d'),
            '\u{00F1}' => result.push('n'),
            '\u{00F2}'..='\u{00F6}' => result.push('o'),
            '\u{00F8}' => result.push('o'),
            '\u{00F9}'..='\u{00FC}' => result.push('u'),
            '\u{00FD}' | '\u{00FF}' => result.push('y'),
            _ => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests;
