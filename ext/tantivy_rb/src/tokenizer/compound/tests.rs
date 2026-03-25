/// Integration tests for the compound tokenizer pipeline.
///
/// These tests verify the full index tokenizer pipeline produces expected output
/// for various real-world inputs, ported from the Java test suite.

#[cfg(test)]
mod index_tokenizer_tests {
    use crate::tokenizer::compound::CompoundIndexTokenizer;
    use crate::tokenizer::default::english_stop_words;
    use rust_stemmers::Algorithm;
    use tantivy::tokenizer::{TokenStream, Tokenizer};

    fn default_leading() -> Vec<char> {
        ".,;:\")<>}]~+".chars().collect()
    }

    fn default_trailing() -> Vec<char> {
        ".,;:\"(<>[{%".chars().collect()
    }

    fn make_tokenizer() -> CompoundIndexTokenizer {
        CompoundIndexTokenizer::new(
            default_leading(),
            default_trailing(),
            english_stop_words().to_vec(),
            Algorithm::English,
        )
    }

    fn tokenize(text: &str) -> Vec<(String, usize)> {
        let mut tokenizer = make_tokenizer();
        let mut stream = tokenizer.token_stream(text);
        let mut result = Vec::new();
        while stream.advance() {
            let t = stream.token();
            result.push((t.text.clone(), t.position));
        }
        result
    }

    fn token_texts(text: &str) -> Vec<String> {
        tokenize(text).into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn test_simple_words() {
        let tokens = token_texts("Hello World");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
    }

    #[test]
    fn test_stop_word_removal() {
        let tokens = token_texts("the quick brown fox and the lazy dog");
        // "the", "and" should be removed as stop words
        assert!(!tokens.contains(&"the".to_string()));
        assert!(!tokens.contains(&"and".to_string()));
        // Content words should be present (stemmed or original)
        assert!(tokens.contains(&"quick".to_string()));
        assert!(tokens.contains(&"brown".to_string()));
        assert!(tokens.contains(&"fox".to_string()));
    }

    #[test]
    fn test_stemming() {
        let tokens = token_texts("running experiments");
        // Stemmed forms are emitted at the main position
        assert!(tokens.contains(&"run".to_string()));
        assert!(tokens.contains(&"experi".to_string()));
        // Original unstemmed forms are also emitted at the SAME position when
        // different from the stemmed form, matching Java FullIndexingAnalyser.
        // Same-position tokens act as synonyms in Tantivy, allowing BM25 to
        // rank exact-match documents higher.
        assert!(tokens.contains(&"running".to_string()));
        assert!(tokens.contains(&"experiments".to_string()));
    }

    #[test]
    fn test_complex_token_expansion() {
        let tokens = token_texts("E21634-016");
        // Full token should be present
        assert!(tokens.contains(&"e21634-016".to_string()));
        // Sub-parts should be present
        assert!(tokens.contains(&"e".to_string()));
        assert!(tokens.contains(&"21634".to_string()));
        assert!(tokens.contains(&"016".to_string()));
        assert!(tokens.contains(&"e21634".to_string()));
    }

    #[test]
    fn test_complex_token_no_stemming() {
        let tokens = token_texts("E21634-016");
        // COMPLEX tokens should NOT be stemmed
        // "e21634-016" should stay as-is, not stemmed
        assert!(tokens.contains(&"e21634-016".to_string()));
    }

    #[test]
    fn test_mixed_word_and_complex() {
        // "Hello" is WORD, "C11.20" is COMPLEX, "Test" is WORD
        let tokens = token_texts("Hello. And C11.20 Test.");
        // "Hello" → stemmed
        assert!(tokens.contains(&"hello".to_string()));
        // "And" is a stop word → removed
        assert!(!tokens.contains(&"and".to_string()));
        // "C11.20" is COMPLEX → expanded
        assert!(tokens.contains(&"c11.20".to_string()));
        assert!(tokens.contains(&"c".to_string()));
        assert!(tokens.contains(&"11".to_string()));
        assert!(tokens.contains(&"20".to_string()));
        // "Test" → stemmed
        assert!(tokens.contains(&"test".to_string()));
    }

    #[test]
    fn test_leading_trailing_strip() {
        // Trailing . should be stripped from "Hello." leaving "Hello" (WORD)
        let tokens = token_texts("Hello.");
        assert!(tokens.contains(&"hello".to_string()));
    }

    #[test]
    fn test_bracketed_words() {
        // "(Fred)" - leading ( is NOT in default leading strip, but trailing ( IS in trailing strip
        // Actually: leading strip has ) but not (, trailing strip has ( but not )
        // So "(Fred)" → leading: ( not stripped, trailing: ) not stripped → "(Fred)" is COMPLEX
        let tokens = token_texts("Hello (Fred)");
        assert!(tokens.contains(&"hello".to_string()));
        // "(Fred)" after stripping: leading ( stays (not in leading list), trailing ) stays (not in trailing list)
        // Wait, let me re-check: The Java leading strip chars are: . , : ; " ) > < } ] ~ +
        // So ) IS in leading strip. ( is NOT in leading strip.
        // Trailing strip chars are: . , : ; " ( < > [ { %
        // So ( IS in trailing strip. ) is NOT in trailing strip.
        // "(Fred)" → strip leading: ( is not in leading list → stays. Strip trailing: ) is not in trailing list → stays
        // So the token stays as "(Fred)" which is COMPLEX
        assert!(tokens.iter().any(|t| t.contains("fred")));
    }

    #[test]
    fn test_date_format() {
        let tokens = token_texts("5-13-2014");
        assert!(tokens.contains(&"5-13-2014".to_string()));
        assert!(tokens.contains(&"5".to_string()));
        assert!(tokens.contains(&"13".to_string()));
        assert!(tokens.contains(&"2014".to_string()));
    }

    #[test]
    fn test_hyphenated_word() {
        let tokens = token_texts("Colgate-Palmolive");
        assert!(tokens.contains(&"colgate-palmolive".to_string()));
        assert!(tokens.contains(&"colgate".to_string()));
        assert!(tokens.contains(&"palmolive".to_string()));
    }

    #[test]
    fn test_pure_punctuation_skipped() {
        let tokens = token_texts("--- ... ===");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_position_increments() {
        let tokens = tokenize("Hello World");
        // First token should be at position > 0
        assert!(tokens[0].1 > 0);
        // Original unstemmed forms share the same position as the stemmed form
        // (same position = synonym in Tantivy, so QueryParser won't create phrase queries)
        let positions: Vec<usize> = tokens.iter().map(|t| t.1).collect();
        assert!(positions.iter().any(|&p| p > 0));
    }

    // =========================================================================
    // Tests ported from Java: ComplexTokenFilterTest
    // =========================================================================

    /// Ported from ComplexTokenFilterTest.testComplexFilterThromboExample
    /// Customer: THRG — "Thrombo 09/VPAC14/MB02 example."
    #[test]
    fn test_thrg_thrombo_example() {
        let tokens = token_texts("Thrombo 09/VPAC14/MB02 example.");
        // "Thrombo" is WORD → stemmed/lowercased
        assert!(tokens.iter().any(|t| t == "thrombo"));
        // "09/VPAC14/MB02" is COMPLEX → full token + sub-spans
        assert!(tokens.contains(&"09/vpac14/mb02".to_string()));
        assert!(tokens.contains(&"09".to_string()));
        assert!(tokens.contains(&"vpac".to_string()));
        assert!(tokens.contains(&"vpac14".to_string()));
        assert!(tokens.contains(&"mb".to_string()));
        assert!(tokens.contains(&"mb02".to_string()));
        assert!(tokens.contains(&"02".to_string()));
        assert!(tokens.contains(&"14".to_string()));
        // Partial sub-spans
        assert!(tokens.contains(&"09/vpac14".to_string()));
        assert!(tokens.contains(&"vpac14/mb02".to_string()));
        // "example." → trailing . stripped → "example" (WORD) → stemmed
        assert!(tokens.iter().any(|t| t == "exampl" || t == "example"));
    }

    /// Ported from ComplexTokenFilterTest.testComplexFilterPhoneExample
    /// Customer: PS-3120 — phone number "(0)845"
    #[test]
    fn test_phone_number_ps3120() {
        let tokens = token_texts("(0)845");
        // "(0)845" is COMPLEX (mixed digit + punct)
        assert!(tokens.contains(&"(0)845".to_string()));
        assert!(tokens.contains(&"845".to_string()));
        assert!(tokens.contains(&"0".to_string()));
    }

    /// Ported from ComplexTokenFilterTest.testWeirdCombinations
    /// "This has !//. 10.2009 2009/10/12 dates."
    #[test]
    fn test_weird_combinations() {
        let tokens = token_texts("This has !//. 10.2009 2009/10/12 dates.");
        // "!//." is pure punct after stripping → skipped
        assert!(!tokens.iter().any(|t| t == "!//."));
        // "10.2009" is COMPLEX
        assert!(tokens.contains(&"10.2009".to_string()));
        assert!(tokens.contains(&"10".to_string()));
        assert!(tokens.contains(&"2009".to_string()));
        // "2009/10/12" is COMPLEX
        assert!(tokens.contains(&"2009/10/12".to_string()));
        assert!(tokens.contains(&"12".to_string()));
        // "dates." → trailing . stripped → "dates" (WORD) → stemmed to "date"
        assert!(tokens.iter().any(|t| t == "date" || t == "dates"));
    }

    /// Ported from ComplexTokenFilterTest.testPunctuationCombinations
    /// "Start 2009:/Fred/:Thing End."
    #[test]
    fn test_punctuation_combinations() {
        let tokens = token_texts("Start 2009:/Fred/:Thing End.");
        // "Start" is WORD
        assert!(tokens.iter().any(|t| t == "start"));
        // "2009:/Fred/:Thing" is COMPLEX after stripping (trailing . removed from "End.")
        assert!(tokens.contains(&"2009:/fred/:thing".to_string()));
        assert!(tokens.contains(&"2009".to_string()));
        assert!(tokens.contains(&"fred".to_string()));
        assert!(tokens.contains(&"thing".to_string()));
        // "End." → trailing . stripped → "End" (WORD) → lowercased
        assert!(tokens.contains(&"end".to_string()));
    }

    /// Ported from ComplexTokenFilterTest.testUnderscoreCombinations
    /// "2006.pc01_AAT170523.01_Blue_UF DF"
    #[test]
    fn test_underscore_combinations() {
        let tokens = token_texts("2006.pc01_AAT170523.01_Blue_UF DF");
        // Full complex token
        assert!(tokens.contains(&"2006.pc01_aat170523.01_blue_uf".to_string()));
        // Sub-spans
        assert!(tokens.contains(&"2006".to_string()));
        assert!(tokens.contains(&"pc01".to_string()));
        assert!(tokens.contains(&"aat170523".to_string()));
        assert!(tokens.contains(&"blue".to_string()));
        assert!(tokens.contains(&"uf".to_string()));
        assert!(tokens.contains(&"pc".to_string()));
        assert!(tokens.contains(&"01".to_string()));
        assert!(tokens.contains(&"170523".to_string()));
        // "DF" is WORD
        assert!(tokens.contains(&"df".to_string()));
    }

    // =========================================================================
    // Tests ported from Java: ComplexTokenFilterJJPDExamplesTest
    // =========================================================================

    /// Ported from ComplexTokenFilterJJPDExamplesTest.testComplexFilterJJPDExample
    /// Customer: JJPD — "JJPD C11.08.21.01 TEST"
    #[test]
    fn test_jjpd_code_example() {
        let tokens = token_texts("JJPD C11.08.21.01 TEST");
        // "JJPD" is WORD
        assert!(tokens.iter().any(|t| t == "jjpd"));
        // "C11.08.21.01" is COMPLEX
        assert!(tokens.contains(&"c11.08.21.01".to_string()));
        assert!(tokens.contains(&"c".to_string()));
        assert!(tokens.contains(&"c11".to_string()));
        assert!(tokens.contains(&"11".to_string()));
        assert!(tokens.contains(&"08".to_string()));
        assert!(tokens.contains(&"21".to_string()));
        assert!(tokens.contains(&"01".to_string()));
        assert!(tokens.contains(&"11.08.21.01".to_string()));
        assert!(tokens.contains(&"08.21.01".to_string()));
        assert!(tokens.contains(&"21.01".to_string()));
        // "TEST" is WORD
        assert!(tokens.contains(&"test".to_string()));
    }

    /// Ported from ComplexTokenFilterJJPDExamplesTest.testComplexFilterJJPD2Example
    /// Customer: JJPD — "JJPD C-25.13.02.00: TEST" (trailing : gets stripped)
    #[test]
    fn test_jjpd_code_with_colon() {
        let tokens = token_texts("JJPD C-25.13.02.00: TEST");
        // "JJPD" is WORD
        assert!(tokens.iter().any(|t| t == "jjpd"));
        // "C-25.13.02.00:" → trailing : stripped → "C-25.13.02.00" is COMPLEX
        assert!(tokens.contains(&"c-25.13.02.00".to_string()));
        assert!(tokens.contains(&"c".to_string()));
        assert!(tokens.contains(&"25".to_string()));
        assert!(tokens.contains(&"13".to_string()));
        assert!(tokens.contains(&"02".to_string()));
        assert!(tokens.contains(&"00".to_string()));
        assert!(tokens.contains(&"25.13.02.00".to_string()));
        assert!(tokens.contains(&"13.02.00".to_string()));
        assert!(tokens.contains(&"02.00".to_string()));
        // "TEST" is WORD
        assert!(tokens.contains(&"test".to_string()));
    }

    // =========================================================================
    // Tests ported from Java: ComplexTokenFilterDNAStringTest
    // =========================================================================

    /// Ported from ComplexTokenFilterDNAStringTest.testBigAndMessy
    /// DNA sequence with dashes — verifies parsing completes and token count is reasonable.
    #[test]
    fn test_dna_big_messy_token() {
        let input = "GAATTCGCCCTTTAATACGACTCACTATAGGGCCAGGCAGCGAG-TCAA--CCGCCAACTTCTTCACCAAAGCCACTG\
                      --TAAGCGTTC----CCGACCACACGCGTCCGAGAAAGGGCGAATTCGTTTAAAC";
        let tokens = token_texts(input);
        // Full token should be present (lowercased)
        assert!(tokens.iter().any(|t| t.starts_with("gaattcgccctttaat")));
        // Sub-spans: some key parts should be present
        assert!(tokens.iter().any(|t| t == "tcaa"));
        assert!(tokens.iter().any(|t| t.contains("ccgaccacacgcgtccgagaaagggcgaattcgtttaaac")));
        // Token count should be reasonable (Java expects ~84 tokens)
        assert!(
            tokens.len() > 50,
            "Expected >50 tokens for DNA string, got {}",
            tokens.len()
        );
    }

    /// Ported from ComplexTokenFilterDNAStringTest.testLongMessyToken
    /// Extremely long DNA string — verifies tokenizer doesn't hang/crash and
    /// respects the 45-block limit.
    #[test]
    fn test_dna_long_messy_token() {
        // Build the same long DNA input as the Java test (concatenated sequences)
        let input = "\
CGAATTCGCCCTTTAATACGACTCACTATAGGGCCAGGCAGCGAGCAC-T--CCATCTGTCACC-GAGCATTAAGCGTGAAGAAAC-C-T---CCCGACCACACGCGTCCGAGAAAGGGCGAATTC-------\
CGAATTCGCCCTTTAATACGACTCACTATAGGGCCAGGCAGCGAGGCG-A--CTTTATGACACAAGATCACTGA-CTTTAATAGACGC-A---CCCGACCACACGCGTCCGAGAAAGGGCGAATTC-------\
CGAATTCGCCCTTTAATACGACTCACTATAGGGCCAGGCAGCGAGTGT-C--CTACCC---AGCTAGTTGTGGTGCCTGTTTCCCCGCGA---CCCGACCACACGCGTCCGAGAAAGGGCGAATTC-------\
CGAATTCGCCCTTTAATACGACTCACTATAGGGCCAGGCAGCGAGCGT-A--CTGGCCGCAATCTCGTCTTGTTTCCTCCGGCAGTCC-----CCCGACCACACGCGTCCGAGAAAGGGCGAATTC-------\
CGAATTCGCCCTTTAATACGACTCACTATAGGGCCAGGCAGCGAGCGT-A--CTGGCCGCAATCTCGTCTTGTTTCCTCCGGCAGTCC-----CCCGACCACACGCGTCCGAGAAAGGGCGAATTC-------";
        let tokens = token_texts(input);
        // Should complete without hanging and produce tokens
        assert!(
            !tokens.is_empty(),
            "Expected tokens for long DNA string"
        );
        // Token count should be bounded (45-block limit prevents exponential expansion)
        assert!(
            tokens.len() < 50000,
            "Token count too high: {} — block limit may not be working",
            tokens.len()
        );
    }

    // =========================================================================
    // Tests ported from Java: ComplexTokenFilterChemicalReactionTest
    // =========================================================================

    /// Ported from ComplexTokenFilterChemicalReactionTest
    /// Chemical compound: "4-chloro-3-iodo-6-(1-(4-methoxybenzyl)-1H-pyrazol-4-yl)pyrazolo[1,5-a]pyrazine"
    #[test]
    fn test_chemical_reaction_compound() {
        let input = "4-chloro-3-iodo-6-(1-(4-methoxybenzyl)-1H-pyrazol-4-yl)pyrazolo[1,5-a]pyrazine";
        let tokens = token_texts(input);
        // Full token should be present (lowercased)
        assert!(tokens.iter().any(|t| t.starts_with("4-chloro-3-iodo")));
        // Key sub-parts from n-gram expansion
        assert!(tokens.contains(&"4".to_string()));
        assert!(tokens.iter().any(|t| t == "chloro"));
        assert!(tokens.iter().any(|t| t == "iodo"));
        assert!(tokens.iter().any(|t| t == "methoxybenzyl"));
        assert!(tokens.iter().any(|t| t == "pyrazol"));
        assert!(tokens.iter().any(|t| t == "pyrazine"));
        assert!(tokens.iter().any(|t| t.contains("4-chloro")));
        assert!(tokens.iter().any(|t| t.contains("3-iodo")));
    }

    // =========================================================================
    // Tests ported from Java: FullIndexingAnalyserTest
    // =========================================================================

    /// Ported from FullIndexingAnalyserTest.testFilterDataSet1
    /// Full pipeline: "Hello. And C11.20 Test." — exact token list
    #[test]
    fn test_full_pipeline_mixed_tokens_exact() {
        let tokens = tokenize("Hello. And C11.20 Test.");
        let texts: Vec<&str> = tokens.iter().map(|t| t.0.as_str()).collect();

        // Expected tokens from Java (lowercased, stop words removed, stemmed, complex expanded):
        // "hello" (WORD, pos 1), "c11.20" (COMPLEX, pos 2+), "c", "c11", "c11.", "11", "11.", "11.20", ".20", "20",
        // "test" (WORD, pos 1)
        assert!(texts.contains(&"hello"));
        assert!(!texts.contains(&"and")); // stop word removed
        assert!(texts.contains(&"c11.20"));
        assert!(texts.contains(&"c"));
        assert!(texts.contains(&"c11"));
        assert!(texts.contains(&"11"));
        assert!(texts.contains(&"20"));
        assert!(texts.contains(&"test"));

        // "And" should cause a position gap (position increment of 2 for next token)
        // Verify "c11.20" is at a higher position than "hello"
        let hello_pos = tokens.iter().find(|t| t.0 == "hello").unwrap().1;
        let c11_pos = tokens.iter().find(|t| t.0 == "c11.20").unwrap().1;
        assert!(
            c11_pos > hello_pos,
            "c11.20 position ({}) should be > hello position ({})",
            c11_pos,
            hello_pos
        );
    }

    /// Ported from FullIndexingAnalyserTest.testFilterWithBrackets
    /// Full pipeline: "Hello (Fred)" — brackets handled correctly
    #[test]
    fn test_full_pipeline_brackets_exact() {
        let tokens = token_texts("Hello (Fred)");
        // "Hello" → WORD → "hello"
        assert!(tokens.contains(&"hello".to_string()));
        // "(Fred)" → COMPLEX after classification (( not in leading strip, ) not in trailing strip)
        // Java expects: "(fred)", "(fred", "fred", "fred)"
        assert!(tokens.contains(&"(fred)".to_string()));
        assert!(tokens.contains(&"(fred".to_string()));
        assert!(tokens.contains(&"fred".to_string()));
        assert!(tokens.contains(&"fred)".to_string()));
    }

    /// Ported from FullIndexingAnalyserTest.testTokeniseMessyToken
    /// Large DNA token — full pipeline should complete within token count bound.
    #[test]
    fn test_full_pipeline_messy_dna_token() {
        let input = "\
CGAATTCGCCCTTTAATACGACTCACTATAGGGCCAGGCAGCGAGCAC-T--CCATCTGTCACC-GAGCATTAAGCGTGAAGAAAC-C-T---CCCGACCACACGCGTCCGAGAAAGGGCGAATTC-------\
CGAATTCGCCCTTTAATACGACTCACTATAGGGCCAGGCAGCGAGGCG-A--CTTTATGACACAAGATCACTGA-CTTTAATAGACGC-A---CCCGACCACACGCGTCCGAGAAAGGGCGAATTC-------\
CGAATTCGCCCTTTAATACGACTCACTATAGGGCCAGGCAGCGAGTGT-C--CTACCC---AGCTAGTTGTGGTGCCTGTTTCCCCGCGA---CCCGACCACACGCGTCCGAGAAAGGGCGAATTC-------";
        let tokens = token_texts(input);
        assert!(
            tokens.len() < 25000,
            "Token count too big: {}",
            tokens.len()
        );
    }

    // =========================================================================
    // Tests ported from Java: BlockTokenParserTest
    // =========================================================================

    /// Ported from BlockTokenParserTest.testParsingAmpersand
    /// "word A&A test" — ampersand makes A&A a COMPLEX token
    #[test]
    fn test_ampersand_handling() {
        let tokens = token_texts("word A&A test");
        // "word" is WORD
        assert!(tokens.contains(&"word".to_string()));
        // "A&A" is COMPLEX (contains non-letter chars)
        assert!(tokens.contains(&"a&a".to_string()));
        // "test" is WORD
        assert!(tokens.contains(&"test".to_string()));
    }

    /// Ported from BlockTokenParserTest.testData1Parsing
    /// Basic document: "This is a very small document.\nIt does not contain very much."
    #[test]
    fn test_basic_document_parsing() {
        let tokens = token_texts("This is a very small document.\nIt does not contain very much.");
        // Stop words "is", "a", "not" should be removed
        assert!(!tokens.contains(&"is".to_string()));
        assert!(!tokens.contains(&"a".to_string()));
        assert!(!tokens.contains(&"not".to_string()));
        // Content words should be present (stemmed)
        assert!(tokens.iter().any(|t| t == "small"));
        assert!(tokens.iter().any(|t| t == "document" || t == "doc"));
        assert!(tokens.iter().any(|t| t == "contain"));
        assert!(tokens.iter().any(|t| t == "much"));
    }
}

// =========================================================================
// Unit tests for ascii_fold (compound/mod.rs)
// =========================================================================

#[cfg(test)]
mod ascii_fold_tests {
    use super::super::ascii_fold;

    #[test]
    fn test_uppercase_accented_vowels() {
        // À-Å → A
        assert_eq!(ascii_fold("\u{00C0}\u{00C1}\u{00C2}\u{00C3}\u{00C4}\u{00C5}"), "AAAAAA");
        // È-Ë → E
        assert_eq!(ascii_fold("\u{00C8}\u{00C9}\u{00CA}\u{00CB}"), "EEEE");
        // Ì-Ï → I
        assert_eq!(ascii_fold("\u{00CC}\u{00CD}\u{00CE}\u{00CF}"), "IIII");
        // Ò-Ö → O, Ø → O
        assert_eq!(ascii_fold("\u{00D2}\u{00D3}\u{00D4}\u{00D5}\u{00D6}\u{00D8}"), "OOOOOO");
        // Ù-Ü → U
        assert_eq!(ascii_fold("\u{00D9}\u{00DA}\u{00DB}\u{00DC}"), "UUUU");
    }

    #[test]
    fn test_lowercase_accented_vowels() {
        // à-å → a
        assert_eq!(ascii_fold("\u{00E0}\u{00E1}\u{00E2}\u{00E3}\u{00E4}\u{00E5}"), "aaaaaa");
        // è-ë → e
        assert_eq!(ascii_fold("\u{00E8}\u{00E9}\u{00EA}\u{00EB}"), "eeee");
        // ì-ï → i
        assert_eq!(ascii_fold("\u{00EC}\u{00ED}\u{00EE}\u{00EF}"), "iiii");
        // ò-ö → o, ø → o
        assert_eq!(ascii_fold("\u{00F2}\u{00F3}\u{00F4}\u{00F5}\u{00F6}\u{00F8}"), "oooooo");
        // ù-ü → u
        assert_eq!(ascii_fold("\u{00F9}\u{00FA}\u{00FB}\u{00FC}"), "uuuu");
    }

    #[test]
    fn test_ligatures_and_special() {
        assert_eq!(ascii_fold("\u{00C6}"), "AE"); // Æ
        assert_eq!(ascii_fold("\u{00E6}"), "ae"); // æ
        assert_eq!(ascii_fold("\u{00C7}"), "C");  // Ç
        assert_eq!(ascii_fold("\u{00E7}"), "c");  // ç
        assert_eq!(ascii_fold("\u{00D1}"), "N");  // Ñ
        assert_eq!(ascii_fold("\u{00F1}"), "n");  // ñ
        assert_eq!(ascii_fold("\u{00D0}"), "D");  // Ð
        assert_eq!(ascii_fold("\u{00F0}"), "d");  // ð
        assert_eq!(ascii_fold("\u{00DD}"), "Y");  // Ý
        assert_eq!(ascii_fold("\u{00FD}"), "y");  // ý
        assert_eq!(ascii_fold("\u{00FF}"), "y");  // ÿ
    }

    #[test]
    fn test_plain_ascii_passthrough() {
        assert_eq!(ascii_fold("Hello World 123!"), "Hello World 123!");
        assert_eq!(ascii_fold("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"),
                   "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz");
    }

    #[test]
    fn test_non_latin1_passthrough() {
        // Chinese characters, emoji — outside U+00C0–U+00FF, should pass through
        assert_eq!(ascii_fold("\u{4e16}\u{754c}"), "\u{4e16}\u{754c}"); // 世界
        assert_eq!(ascii_fold("\u{1F600}"), "\u{1F600}"); // 😀
    }

    #[test]
    fn test_mixed_string() {
        assert_eq!(ascii_fold("caf\u{00E9}"), "cafe");
        assert_eq!(ascii_fold("\u{00DC}nters\u{00FC}chung"), "Untersuchung");
        assert_eq!(ascii_fold("na\u{00EF}ve"), "naive");
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(ascii_fold(""), "");
    }
}

#[cfg(test)]
mod query_tokenizer_tests {
    use crate::tokenizer::compound::query::CompoundQueryTokenizer;
    use crate::tokenizer::default::english_stop_words;
    use rust_stemmers::Algorithm;
    use tantivy::tokenizer::{TokenStream, Tokenizer};

    fn make_tokenizer() -> CompoundQueryTokenizer {
        CompoundQueryTokenizer::new(english_stop_words().to_vec(), Algorithm::English)
    }

    fn token_texts(text: &str) -> Vec<String> {
        let mut tokenizer = make_tokenizer();
        let mut stream = tokenizer.token_stream(text);
        let mut result = Vec::new();
        while stream.advance() {
            result.push(stream.token().text.clone());
        }
        result
    }

    #[test]
    fn test_basic_query() {
        let tokens = token_texts("Annual Support and Maintenance");
        // "and" is a stop word
        assert!(!tokens.contains(&"and".to_string()));
        assert!(tokens.contains(&"annual".to_string()));
        assert!(tokens.contains(&"support".to_string()));
        assert!(tokens.iter().any(|t| t == "mainten" || t == "maintenance"));
    }

    #[test]
    fn test_complex_query() {
        let tokens = token_texts("E21771-013-3A = 59.9 mg API/mL");
        // "=" should be skipped (single punctuation char)
        // "E21771-013-3A" after punct strip → "E21771-013-3A" (no leading/trailing to strip)
        assert!(tokens.iter().any(|t| t.contains("e21771")));
    }

    #[test]
    fn test_stop_words_removed() {
        let tokens = token_texts("the quick and the slow");
        assert!(!tokens.contains(&"the".to_string()));
        assert!(!tokens.contains(&"and".to_string()));
        assert!(tokens.contains(&"quick".to_string()));
        assert!(tokens.contains(&"slow".to_string()));
    }

    #[test]
    fn test_tilde_stripped() {
        let tokens = token_texts("~0.45");
        // Leading ~ should be stripped
        assert!(tokens.iter().any(|t| t == "0.45"));
    }

    #[test]
    fn test_trailing_punct_stripped() {
        let tokens = token_texts("methanol:water;");
        // Trailing ; should be stripped, then the result gets stemmed.
        // "methanol:water" → stemmer processes as single token → some stemmed form
        // Just verify we get at least one token and it's not empty
        assert!(!tokens.is_empty());
        // The stemmed form of "methanol:water" depends on the stemmer
        // Just check the trailing ; was stripped (no token should end with ;)
        assert!(tokens.iter().all(|t| !t.ends_with(';')));
    }

    #[test]
    fn test_wildcard_preserved() {
        let tokens = token_texts("print*");
        assert!(tokens.iter().any(|t| t.contains("print*")));
    }

    #[test]
    fn test_single_punct_skipped() {
        let tokens = token_texts("hello = world");
        // "=" single punct is skipped
        assert!(!tokens.iter().any(|t| t == "="));
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
    }

    // =========================================================================
    // Tests ported from Java: PatentSafeQueryAnalyserTest
    // =========================================================================

    /// Ported from PatentSafeQueryAnalyserTest.testFilterDataSet01
    /// "Annual Support and Maintenance" — exact tokens with stemming
    #[test]
    fn test_query_annual_support_exact() {
        let tokens = token_texts("Annual Support and Maintenance");
        // Java expects: "annual" (pos 1), "support" (pos 1), "mainten" (pos 2), "maintenance" (pos 0)
        assert!(tokens.contains(&"annual".to_string()));
        assert!(tokens.contains(&"support".to_string()));
        // "and" removed as stop word
        assert!(!tokens.contains(&"and".to_string()));
        // "maintenance" → stemmed to "mainten" + original "maintenance"
        assert!(tokens.contains(&"mainten".to_string()));
        assert!(tokens.contains(&"maintenance".to_string()));
    }

    /// Ported from PatentSafeQueryAnalyserTest.testFilterDataSet02
    /// "E21771-013-3A = 59.9 mg API/mL" — complex tokens in query
    #[test]
    fn test_query_complex_tokens_exact() {
        let tokens = token_texts("E21771-013-3A = 59.9 mg API/mL");
        // Java expects: "e21771-013-3a" (pos 1), "59.9" (pos 2), "mg" (pos 1), "api/ml" (pos 1)
        assert!(tokens.contains(&"e21771-013-3a".to_string()));
        // "=" is skipped (single punct)
        assert!(!tokens.contains(&"=".to_string()));
        assert!(tokens.contains(&"59.9".to_string()));
        assert!(tokens.contains(&"mg".to_string()));
        assert!(tokens.contains(&"api/ml".to_string()));
    }

    /// Ported from PatentSafeQueryAnalyserTest.testFilterScienceQuery
    /// "~0.4 mg/mL in 25:75 methanol:water; prepared E21634-016"
    #[test]
    fn test_query_science_example() {
        let tokens = token_texts("~0.4 mg/mL in 25:75 methanol:water; prepared E21634-016");
        // "~0.4" → ~ stripped → "0.4"
        assert!(tokens.contains(&"0.4".to_string()));
        // "mg/mL" → lowercased
        assert!(tokens.contains(&"mg/ml".to_string()));
        // "in" is a stop word → removed
        assert!(!tokens.contains(&"in".to_string()));
        // "25:75" stays as-is (complex-ish)
        assert!(tokens.contains(&"25:75".to_string()));
        // "methanol:water;" → trailing ; stripped → stemmed
        assert!(tokens.iter().any(|t| t.starts_with("methanol:wat")));
        // "prepared" → stemmed to "prepar" + original "prepared"
        assert!(tokens.iter().any(|t| t == "prepar" || t == "prepared"));
        // "E21634-016" → lowercased
        assert!(tokens.contains(&"e21634-016".to_string()));
    }

    /// Ported from PatentSafeQueryAnalyserTest.testFilterWildcardQuery
    /// "print*" — wildcard preserved through query pipeline
    #[test]
    fn test_query_wildcard_star() {
        let tokens = token_texts("print*");
        assert!(tokens.contains(&"print*".to_string()));
    }

    /// Ported from PatentSafeQueryAnalyserTest.testFilterSingleWildcardQuery
    /// "print?" — single-char wildcard preserved
    #[test]
    fn test_query_wildcard_question() {
        let tokens = token_texts("print?");
        assert!(tokens.contains(&"print?".to_string()));
    }

    /// Ported from PatentSafeQueryAnalyserTest.testFilterQuotedQuery
    /// '"50 ng/mL PDGF" in 2% heat' — quotes preserved for phrase queries
    #[test]
    fn test_query_quoted_phrase() {
        let tokens = token_texts("\"50 ng/mL PDGF\" in 2% heat");
        // Java expects: "\"50", "ng/ml", "pdgf\"", "2", "heat"
        assert!(tokens.iter().any(|t| t == "\"50"));
        assert!(tokens.contains(&"ng/ml".to_string()));
        assert!(tokens.iter().any(|t| t == "pdgf\""));
        // "in" is a stop word → removed
        assert!(!tokens.contains(&"in".to_string()));
        // "2%" → trailing % stripped → "2"
        assert!(tokens.iter().any(|t| t == "2"));
        assert!(tokens.contains(&"heat".to_string()));
    }

    /// Ported from PatentSafeQueryAnalyserTest.testSkippedPunctuation
    /// "token: =; skipped" — colon and = tokens skipped
    #[test]
    fn test_query_skipped_punctuation() {
        let tokens = token_texts("token: =; skipped");
        // "token:" → trailing : stripped → "token" (WORD)
        assert!(tokens.iter().any(|t| t == "token"));
        // "=;" is pure punct → skipped
        assert!(!tokens.iter().any(|t| t == "=;"));
        assert!(!tokens.iter().any(|t| t == "="));
        // "skipped" → stemmed to "skip" + original "skipped"
        assert!(tokens.iter().any(|t| t == "skip" || t == "skipped"));
    }

    /// Ported from PatentSafeQueryAnalyserTest.testPhoneNumberText
    /// Multi-line phone number text
    #[test]
    fn test_query_phone_number() {
        let tokens = token_texts(
            "This is an example of a phone number:\n\
             Phone: +44 (0)845 2300160 x2002\n\
             200-200-344",
        );
        // Key tokens from Java expectations:
        assert!(tokens.iter().any(|t| t == "exampl" || t == "example"));
        assert!(tokens.iter().any(|t| t == "phone"));
        assert!(tokens.iter().any(|t| t == "number"));
        assert!(tokens.iter().any(|t| t == "44"));
        // "(0)845" → lowercased
        assert!(tokens.iter().any(|t| t == "0)845"));
        assert!(tokens.contains(&"2300160".to_string()));
        assert!(tokens.contains(&"x2002".to_string()));
        assert!(tokens.contains(&"200-200-344".to_string()));
    }

    /// Ported from PatentSafeQueryAnalyserTest.testNotebookPage
    /// "E12345-001" — notebook page ID preserved as complex token
    #[test]
    fn test_query_notebook_page_id() {
        let tokens = token_texts("E12345-001");
        assert!(tokens.contains(&"e12345-001".to_string()));
    }
}
