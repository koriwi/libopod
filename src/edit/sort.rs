//! Shared iTunes-style sort-key helpers used by the binary and `SQLite` writers.

/// Case-folded, article-stripped sort key (approximation of iTunes ordering).
pub(crate) fn sort_key(text: &str) -> String {
    strip_article(text)
        .chars()
        .flat_map(char::to_lowercase)
        .collect()
}

/// Strips a leading English article (`A `, `An `, `The `) for sorting.
pub(crate) fn strip_article(text: &str) -> &str {
    if !text.starts_with(['a', 'A', 't', 'T']) {
        return text;
    }
    let lower = text.to_lowercase();
    for article in ["a ", "an ", "the "] {
        if lower.starts_with(article) {
            return text[article.len()..].trim_start();
        }
    }
    text
}

/// First alphanumeric character used for type-53 jump-table grouping.
pub(crate) fn jump_letter(text: &str) -> u16 {
    for character in text.chars() {
        if character.is_alphanumeric() {
            if character.is_ascii_digit() {
                return u16::from(b'0');
            }
            let upper = character.to_uppercase().next().unwrap_or(character);
            if let Ok(code) = u16::try_from(u32::from(upper)) {
                return code;
            }
            return u16::from(b'0');
        }
    }
    u16::from(b'0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_english_articles() {
        assert_eq!(strip_article("The Beatles"), "Beatles");
        assert_eq!(strip_article("A Day in the Life"), "Day in the Life");
        assert_eq!(strip_article("An American in Paris"), "American in Paris");
        assert_eq!(strip_article("Theremin"), "Theremin");
        assert_eq!(strip_article("Album"), "Album");
    }

    #[test]
    fn computes_consistent_jump_letters() {
        assert_eq!(jump_letter(""), u16::from(b'0'));
        assert_eq!(jump_letter("1234"), u16::from(b'0'));
        assert_eq!(jump_letter("Zebra"), u16::from(b'Z'));
        assert_eq!(jump_letter("(hello)"), u16::from(b'H'));
    }
}
