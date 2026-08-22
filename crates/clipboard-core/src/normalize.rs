use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

pub fn normalize_search_text(value: &str) -> String {
    value
        .to_lowercase()
        .replace('ł', "l")
        .nfd()
        .filter(|ch| !is_combining_mark(*ch))
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::normalize_search_text;

    #[test]
    fn normalizes_polish_l_and_diacritics_for_search() {
        assert_eq!(normalize_search_text("ŁÓDŹ i łąka"), "lodz i laka");
    }
}
