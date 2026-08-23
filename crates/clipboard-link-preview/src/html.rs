//! Reading a page's title and icon out of its markup.
//!
//! Deliberately not a parser. This looks for two things in a bounded prefix of
//! the document and stops; a clipboard preview does not need to know what the
//! page means, and a full parser would be a much larger thing to trust with
//! somebody else's bytes.

/// Longest title kept. Anything past this is a page abusing the field.
const MAX_TITLE_CHARS: usize = 200;

/// What the markup offered.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PageMetadata {
    pub title: Option<String>,
    /// The icon reference exactly as the page wrote it, still relative.
    pub icon_href: Option<String>,
}

/// Pulls the title and the icon reference out of a document.
pub fn read_metadata(document: &str) -> PageMetadata {
    PageMetadata {
        title: read_title(document).map(|title| collapse_whitespace(&title)),
        icon_href: read_icon_href(document),
    }
}

fn read_title(document: &str) -> Option<String> {
    let lowered = document.to_ascii_lowercase();
    let open = lowered.find("<title")?;
    let content_start = open + document[open..].find('>')? + 1;
    let close = lowered[content_start..].find("</title>")? + content_start;
    let title = decode_entities(&document[content_start..close]);
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_TITLE_CHARS).collect())
}

/// Finds the first `<link>` that declares itself an icon.
fn read_icon_href(document: &str) -> Option<String> {
    let lowered = document.to_ascii_lowercase();
    let mut cursor = 0_usize;
    while let Some(offset) = lowered[cursor..].find("<link") {
        let start = cursor + offset;
        let length = lowered[start..].find('>')?;
        let tag = &document[start..start + length];
        let lowered_tag = &lowered[start..start + length];
        cursor = start + length + 1;
        let Some(rel) = read_attribute(tag, lowered_tag, "rel") else {
            continue;
        };
        let is_icon = rel
            .split_ascii_whitespace()
            .any(|word| word.eq_ignore_ascii_case("icon"));
        if !is_icon {
            continue;
        }
        if let Some(href) = read_attribute(tag, lowered_tag, "href") {
            let href = href.trim();
            if !href.is_empty() {
                return Some(decode_entities(href));
            }
        }
    }
    None
}

/// Reads one attribute's value out of a tag, quoted or bare.
fn read_attribute(tag: &str, lowered_tag: &str, name: &str) -> Option<String> {
    let mut cursor = 0_usize;
    loop {
        let offset = lowered_tag[cursor..].find(name)? + cursor;
        let after = offset + name.len();
        // Must be a whole attribute name, not the tail of another one.
        let preceded_by_space = offset == 0
            || lowered_tag[..offset]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_ascii_whitespace());
        let rest = lowered_tag[after..].trim_start();
        if preceded_by_space && rest.starts_with('=') {
            let value_start = after + (lowered_tag[after..].len() - rest.len()) + 1;
            return Some(read_value(&tag[value_start..]));
        }
        cursor = after;
    }
}

fn read_value(rest: &str) -> String {
    let rest = rest.trim_start();
    let mut characters = rest.chars();
    match characters.next() {
        Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or_default().to_owned(),
        _ => rest
            .split(|character: char| character.is_ascii_whitespace() || character == '>')
            .next()
            .unwrap_or_default()
            .to_owned(),
    }
}

/// Decodes the handful of entities a title realistically contains.
fn decode_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_read_and_tidied() {
        let document = "<html><head><TITLE>\n  Synthetic   Page\n</TITLE></head></html>";

        assert_eq!(
            read_metadata(document).title.as_deref(),
            Some("Synthetic Page")
        );
    }

    #[test]
    fn entities_in_a_title_become_the_characters_they_stand_for() {
        let document = "<title>Tom &amp; Jerry &lt;live&gt; &quot;now&quot;</title>";

        assert_eq!(
            read_metadata(document).title.as_deref(),
            Some("Tom & Jerry <live> \"now\"")
        );
    }

    #[test]
    fn a_declared_icon_is_preferred_over_guessing() {
        for document in [
            r#"<link rel="icon" href="/assets/icon.png">"#,
            r#"<link href='/assets/icon.png' rel='shortcut icon'>"#,
            r#"<link rel=icon href=/assets/icon.png>"#,
            r#"<link rel="apple-touch-icon" href="/other.png"><link rel="icon" href="/assets/icon.png">"#,
        ] {
            assert_eq!(
                read_metadata(document).icon_href.as_deref(),
                Some("/assets/icon.png"),
                "{document}"
            );
        }
    }

    #[test]
    fn a_stylesheet_link_is_not_mistaken_for_an_icon() {
        let document = r#"<link rel="stylesheet" href="/style.css">"#;

        assert_eq!(read_metadata(document).icon_href, None);
    }

    #[test]
    fn a_page_with_neither_yields_neither_rather_than_something_invented() {
        assert_eq!(
            read_metadata("<html><body>nothing</body></html>"),
            PageMetadata::default()
        );
        assert_eq!(read_metadata("<title>   </title>").title, None);
    }

    #[test]
    fn a_title_longer_than_the_cap_is_cut_rather_than_kept() {
        let document = format!("<title>{}</title>", "x".repeat(MAX_TITLE_CHARS + 50));

        assert_eq!(
            read_metadata(&document)
                .title
                .map(|title| title.chars().count()),
            Some(MAX_TITLE_CHARS)
        );
    }

    #[test]
    fn an_unterminated_tag_stops_the_scan_instead_of_running_away() {
        assert_eq!(read_metadata("<title>never closed").title, None);
        assert_eq!(
            read_metadata("<link rel=\"icon\" href=\"/a.png\"").icon_href,
            None
        );
    }
}
