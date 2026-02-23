/// Token classification, ported from Java's BlockTokenParsingFilter.
///
/// After stripping configurable leading/trailing punctuation, tokens are classified as:
/// - WORD: entirely Unicode letters → gets stemmed + stop-word filtered
/// - COMPLEX: mixed characters with at least one letter or digit → gets n-gram expanded
/// - Skip: pure punctuation or empty → dropped

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Complex,
    Skip,
}

/// Strip configurable leading and trailing punctuation characters from a token.
///
/// Ported from Java BlockTokenParsingFilter.isRemovableStartCharacter / isRemovableEndCharacter.
pub fn strip_punctuation(token: &str, leading: &[char], trailing: &[char]) -> String {
    let chars: Vec<char> = token.chars().collect();
    let mut start = 0;
    let mut end = chars.len();

    while start < end && is_removable_start(chars[start], leading) {
        start += 1;
    }
    while end > start && is_removable_end(chars[end - 1], trailing) {
        end -= 1;
    }

    chars[start..end].iter().collect()
}

/// Classify a (already-stripped) token as WORD, COMPLEX, or Skip.
///
/// Ported from Java BlockTokenParsingFilter.incrementToken() classification logic.
pub fn classify_token(token: &str) -> TokenKind {
    if token.is_empty() {
        return TokenKind::Skip;
    }

    let mut letter_count = 0usize;
    let mut number_count = 0usize;
    let mut other_count = 0usize;

    for c in token.chars() {
        if c.is_alphabetic() {
            letter_count += 1;
        } else if c.is_ascii_digit() {
            number_count += 1;
        } else {
            other_count += 1;
        }
    }

    let total = letter_count + number_count + other_count;

    // If just letters, it's a WORD
    if letter_count == total {
        return TokenKind::Word;
    }

    // If just messy other characters, or no letters and no numbers: skip
    if other_count == total || (letter_count == 0 && number_count == 0) {
        return TokenKind::Skip;
    }

    // Otherwise it's COMPLEX
    TokenKind::Complex
}

/// Check if a character is removable from the start of a token.
/// Default set matches Java: . , : ; " ) > < } ] ~ +
fn is_removable_start(c: char, custom_chars: &[char]) -> bool {
    if c.is_alphanumeric() {
        return false;
    }
    if !custom_chars.is_empty() {
        return custom_chars.contains(&c);
    }
    // Default leading strip characters (from Java BlockTokenParsingFilter)
    matches!(c, '.' | ',' | ':' | ';' | '"' | ')' | '>' | '<' | '}' | ']' | '~' | '+')
}

/// Check if a character is removable from the end of a token.
/// Default set matches Java: . , : ; " ( < > [ { %
fn is_removable_end(c: char, custom_chars: &[char]) -> bool {
    if c.is_alphanumeric() {
        return false;
    }
    if !custom_chars.is_empty() {
        return custom_chars.contains(&c);
    }
    // Default trailing strip characters (from Java BlockTokenParsingFilter)
    matches!(c, '.' | ',' | ':' | ';' | '"' | '(' | '<' | '>' | '[' | '{' | '%')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_word() {
        assert_eq!(classify_token("hello"), TokenKind::Word);
        assert_eq!(classify_token("Hello"), TokenKind::Word);
        assert_eq!(classify_token("café"), TokenKind::Word); // non-ASCII letters
    }

    #[test]
    fn test_classify_complex() {
        assert_eq!(classify_token("E21634-016"), TokenKind::Complex);
        assert_eq!(classify_token("09/VPAC14/MB02"), TokenKind::Complex);
        assert_eq!(classify_token("C11.20"), TokenKind::Complex);
        assert_eq!(classify_token("5-13-2014"), TokenKind::Complex);
    }

    #[test]
    fn test_classify_skip() {
        assert_eq!(classify_token(""), TokenKind::Skip);
        assert_eq!(classify_token("---"), TokenKind::Skip);
        assert_eq!(classify_token("..."), TokenKind::Skip);
    }

    #[test]
    fn test_strip_default_leading() {
        let leading = vec!['.', ',', ':', ';', '"', ')', '>', '<', '}', ']', '~', '+'];
        let trailing = vec!['.', ',', ':', ';', '"', '(', '<', '>', '[', '{', '%'];
        assert_eq!(strip_punctuation("~hello", &leading, &trailing), "hello");
        assert_eq!(strip_punctuation("+test", &leading, &trailing), "test");
        assert_eq!(strip_punctuation("hello.", &leading, &trailing), "hello");
        // ( not in leading strip, ) not in trailing strip → both stay
        assert_eq!(strip_punctuation("(hello)", &leading, &trailing), "(hello)");
        // Leading ) IS stripped, trailing ( IS stripped
        assert_eq!(strip_punctuation(")hello(", &leading, &trailing), "hello");
    }

    #[test]
    fn test_strip_empty_result() {
        let leading = vec!['.', ','];
        let trailing = vec!['.', ','];
        assert_eq!(strip_punctuation(".,.,", &leading, &trailing), "");
    }
}
