//! N-gram expansion for COMPLEX tokens.
//!
//! Ported from Java's ComplexTokenFilter. Breaks a complex token into
//! character-type blocks (LETTER, NUMBER, OTHER) and generates sub-span
//! combinations. All sub-spans are emitted at the same position as the full
//! token, so they act as synonyms in the index.
//!
//! Example: `"PROJ/ENG03:40"` produces:
//!   - Full token: `"proj/eng03:40"`
//!   - Sub-spans: `"proj"`, `"proj/"`, `"proj/eng"`, `"proj/eng03"`, ...,
//!     `"eng"`, `"eng03"`, ..., `"03"`, `"03:40"`, `"40"`, etc.
//!
//! Bounded by `MAX_TOKEN_LENGTH` (100 chars) and `MAX_TOKEN_BLOCKS` (45 blocks)
//! to prevent exponential expansion on pathological inputs like DNA sequences.

/// Maximum length of a generated sub-token.
const MAX_TOKEN_LENGTH: usize = 100;
/// Maximum number of blocks to combine from a single starting block.
const MAX_TOKEN_BLOCKS: usize = 45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharType {
    Letter,
    Number,
    Other,
}

impl CharType {
    fn determine(c: char) -> CharType {
        if c.is_alphabetic() {
            CharType::Letter
        } else if c.is_ascii_digit() {
            CharType::Number
        } else {
            CharType::Other
        }
    }
}

/// A contiguous run of characters sharing the same `CharType`.
///
/// Used internally by the n-gram expander to split tokens into typed blocks
/// before generating sub-span combinations.
#[derive(Debug, Clone)]
struct CharBlock {
    /// Index into the `Vec<char>` where this block starts (inclusive).
    /// Note: this is a char index, not a byte offset.
    start: usize,
    /// Index into the `Vec<char>` where this block ends (exclusive).
    /// Note: this is a char index, not a byte offset.
    end: usize,
    /// The character type shared by all characters in this block.
    char_type: CharType,
}

impl CharBlock {
    fn is_other(&self) -> bool {
        self.char_type == CharType::Other
    }
}

/// Parse a character slice into character-type blocks.
///
/// Each contiguous run of the same character type becomes one block, except that
/// OTHER characters always form their own boundary (like in the Java impl where
/// OTHER triggers a new block at every character boundary change).
fn parse_as_blocks(chars: &[char]) -> Vec<CharBlock> {
    if chars.is_empty() {
        return Vec::new();
    }

    let mut blocks = Vec::new();
    let mut base_offset = 0;
    let mut base_type = CharType::determine(chars[0]);

    for next_offset in 1..chars.len() {
        let next_type = CharType::determine(chars[next_offset]);
        // Change block at any OTHER boundary, or a change of type
        if next_type == CharType::Other || base_type != next_type {
            blocks.push(CharBlock {
                start: base_offset,
                end: next_offset,
                char_type: base_type,
            });
            base_offset = next_offset;
            base_type = next_type;
        }
    }
    blocks.push(CharBlock {
        start: base_offset,
        end: chars.len(),
        char_type: base_type,
    });

    blocks
}

/// Check if a range of blocks contains at least one non-OTHER block.
fn is_valid_token_range(blocks: &[CharBlock], start: usize, end: usize) -> bool {
    for block in &blocks[start..=end] {
        if !block.is_other() {
            return true;
        }
    }
    false
}

/// Expand a complex token into n-gram sub-spans.
///
/// Returns a vector of strings. The first element is always the full token.
/// Subsequent elements are sub-spans at position 0 relative to the full token.
pub fn expand_complex_token(token: &str) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();

    // Always start with the full token
    result.push(token.to_string());

    let blocks = parse_as_blocks(&chars);

    for start_idx in 0..blocks.len() {
        let start_block = &blocks[start_idx];

        // Always inject any non-OTHER first block
        if !start_block.is_other() {
            let sub: String = chars[start_block.start..start_block.end].iter().collect();
            result.push(sub);
        }

        let mut block_count = 1usize;
        while block_count < MAX_TOKEN_BLOCKS {
            let end_idx = start_idx + block_count;
            if end_idx >= blocks.len() {
                break;
            }

            let end_block_end = blocks[end_idx].end;
            let block_length = end_block_end - start_block.start;
            if block_length > MAX_TOKEN_LENGTH {
                break;
            }

            // Skip duplicate of the full token
            if start_idx == 0 && block_length == chars.len() {
                break;
            }

            // Only emit if the range contains at least one non-OTHER block
            if is_valid_token_range(&blocks, start_idx, end_idx) {
                let sub: String = chars[start_block.start..end_block_end].iter().collect();
                result.push(sub);
            }

            block_count += 1;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_blocks_simple() {
        let chars: Vec<char> = "abc123".chars().collect();
        let blocks = parse_as_blocks(&chars);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].char_type, CharType::Letter);
        assert_eq!(blocks[0].start, 0);
        assert_eq!(blocks[0].end, 3);
        assert_eq!(blocks[1].char_type, CharType::Number);
        assert_eq!(blocks[1].start, 3);
        assert_eq!(blocks[1].end, 6);
    }

    #[test]
    fn test_parse_blocks_with_separator() {
        let chars: Vec<char> = "abc/def".chars().collect();
        let blocks = parse_as_blocks(&chars);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].char_type, CharType::Letter);
        assert_eq!(blocks[1].char_type, CharType::Other);
        assert_eq!(blocks[2].char_type, CharType::Letter);
    }

    #[test]
    fn test_expand_simple() {
        let expanded = expand_complex_token("e21634-016");
        // Full token first
        assert_eq!(expanded[0], "e21634-016");
        // Should contain sub-parts
        assert!(expanded.contains(&"e".to_string()));
        assert!(expanded.contains(&"21634".to_string()));
        assert!(expanded.contains(&"016".to_string()));
        assert!(expanded.contains(&"e21634".to_string()));
    }

    #[test]
    fn test_expand_date() {
        let expanded = expand_complex_token("5-13-2014");
        assert_eq!(expanded[0], "5-13-2014");
        assert!(expanded.contains(&"5".to_string()));
        assert!(expanded.contains(&"13".to_string()));
        assert!(expanded.contains(&"2014".to_string()));
    }
}
