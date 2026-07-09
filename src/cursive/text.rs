/// Normalize text for rendering with a font that lacks œ/Œ ligature glyphs
/// (verified: EMS Allure has full Latin-1 coverage but no Latin-9 œ/Œ).
/// All other characters pass through unchanged; unknown characters are
/// left as-is here and skipped later at layout time.
pub fn normalize(input: &str) -> String {
    input
        .chars()
        .flat_map(|c| match c {
            'œ' => vec!['o', 'e'],
            'Œ' => vec!['O', 'E'],
            other => vec![other],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_lowercase_oe_ligature() {
        assert_eq!(normalize("cœur"), "coeur");
    }

    #[test]
    fn substitutes_uppercase_oe_ligature() {
        assert_eq!(normalize("ŒUF"), "OEUF");
    }

    #[test]
    fn leaves_accented_characters_untouched() {
        assert_eq!(normalize("Où étais-tu ?"), "Où étais-tu ?");
    }

    #[test]
    fn leaves_plain_ascii_untouched() {
        assert_eq!(normalize("Hello, world!"), "Hello, world!");
    }
}
