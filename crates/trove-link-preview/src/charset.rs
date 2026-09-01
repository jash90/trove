//! Turning the bytes a page sent into the text they mean.
//!
//! The declaration of a page's encoding lives inside the page, which is the
//! classic chicken and egg: the meta tag naming the encoding has to be read
//! before the text is decoded. The way out is that every encoding a browser
//! would honour keeps plain ASCII intact, so scanning the raw bytes for
//! `<meta charset>` finds the name even while nothing else about the bytes
//! is understood yet.
//!
//! Polish pages make this worth doing rather than merely correct: CMSes from
//! before UTF-8 won still ship windows-1250 and ISO-8859-2, and decoding
//! those as UTF-8 replaces every diacritic with the replacement character.

use encoding_rs::Encoding;

/// How far into a document the encoding declaration may sit.
///
/// The convention is that it comes first; browsers scan roughly this far and
/// so does everyone else.
const DECLARATION_WINDOW_BYTES: usize = 2048;

/// Picks an encoding the way a browser would, then decodes with it.
///
/// The order matters. A header names bytes as they travelled; a byte-order
/// mark overrides everything because it travels with the bytes; a meta tag
/// is what the page said about itself; and UTF-8 is what everything else
/// turns out to be.
pub fn decode(raw: &[u8], content_type: Option<&str>) -> String {
    let encoding = content_type
        .and_then(from_content_type)
        .or_else(|| from_byte_order_mark(raw))
        .or_else(|| from_declaration(raw))
        .unwrap_or(encoding_rs::UTF_8);
    // `decode` honours a byte-order mark even when another encoding was
    // chosen, strips it, and never fails: unknown bytes become U+FFFD, which
    // is the honest rendering of bytes this cannot name.
    let (text, _, _) = encoding.decode(raw);
    text.into_owned()
}

/// Reads the `charset` parameter out of a `Content-Type` header.
fn from_content_type(content_type: &str) -> Option<&'static Encoding> {
    for parameter in content_type.split(';').skip(1) {
        let parameter = parameter.trim();
        let Some(value) = parameter.strip_prefix("charset=") else {
            continue;
        };
        if let Some(encoding) = Encoding::for_label(value.trim().trim_matches('"').as_bytes()) {
            return Some(encoding);
        }
    }
    None
}

/// Recognises the marks that say which Unicode transformation follows.
fn from_byte_order_mark(raw: &[u8]) -> Option<&'static Encoding> {
    Encoding::for_bom(raw).map(|(encoding, _)| encoding)
}

/// Scans the document's opening bytes for a declaration of its own.
///
/// Both spellings occur: `<meta charset="…">`, the modern one, and the older
/// `<meta http-equiv="Content-Type" content="…; charset=…">`. Names are ASCII
/// in every encoding honoured here, so the window is scanned lossily — bytes
/// outside ASCII become placeholders that simply never match a tag.
fn from_declaration(raw: &[u8]) -> Option<&'static Encoding> {
    let window = &raw[..raw.len().min(DECLARATION_WINDOW_BYTES)];
    let lowered = String::from_utf8_lossy(window).to_ascii_lowercase();

    for offset in find_all(&lowered, "<meta") {
        let Some(close) = lowered[offset..].find('>') else {
            break;
        };
        let tag = &lowered[offset..offset + close];
        if let Some(charset) = attribute_value(tag, "charset")
            && let Some(encoding) = Encoding::for_label(charset.as_bytes())
        {
            return Some(encoding);
        }
        // The http-equiv form carries its charset inside `content`, after a
        // semicolon, exactly as the header it imitates.
        let declares_content_type = attribute_value(tag, "http-equiv")
            .is_some_and(|value| value.eq_ignore_ascii_case("content-type"));
        if declares_content_type
            && let Some(content) = attribute_value(tag, "content")
            && let Some(position) = content.to_ascii_lowercase().find("charset=")
        {
            let value = content[position + "charset=".len()..].split(';').next()?;
            if let Some(encoding) = Encoding::for_label(value.trim().trim_matches('"').as_bytes()) {
                return Some(encoding);
            }
        }
    }
    None
}

/// Offsets of every occurrence of one needle.
fn find_all<'a>(haystack: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    let mut cursor = 0_usize;
    std::iter::from_fn(move || {
        let offset = haystack.get(cursor..)?.find(needle)?;
        let found = cursor + offset;
        cursor = found + needle.len();
        Some(found)
    })
}

/// Reads one attribute's value from an already-lower-cased tag fragment.
fn attribute_value(tag: &str, name: &str) -> Option<String> {
    for offset in find_all(tag, name) {
        let after = offset + name.len();
        let preceded_by_space = offset == 0
            || tag[..offset]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_ascii_whitespace());
        let rest = tag[after..].trim_start();
        if !(preceded_by_space && rest.starts_with('=')) {
            continue;
        }
        let value = rest[1..].trim_start();
        let mut characters = value.chars();
        return match characters.next() {
            Some(quote @ ('"' | '\'')) => Some(
                value[1..]
                    .split(quote)
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            _ => Some(
                value
                    .split_ascii_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            ),
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS_1250_GREETING: &[u8] =
        &[b'<', b't', b'i', b't', b'l', b'e', b'>', 0xB3, b'/', b'>'];

    #[test]
    fn a_header_declaration_decides_before_the_page_does() {
        // Byte 0xA5 reads as Ą under windows-1250 but as Ľ under the
        // iso-8859-2 the page declares. The header names the truth about how
        // these bytes travelled, so it wins.
        let mut raw = b"<meta charset=\"iso-8859-2\"><title>".to_vec();
        raw.push(0xA5);
        raw.extend_from_slice(b"</title>");

        assert_eq!(
            decode(&raw, Some("text/html; charset=windows-1250")),
            "<meta charset=\"iso-8859-2\"><title>Ą</title>"
        );
    }

    #[test]
    fn a_page_declaring_windows_1250_decodes_polish_diacritics() {
        let mut raw = b"<html><head><meta charset=\"windows-1250\"><title>".to_vec();
        raw.extend_from_slice(&[0xB3]); // ł in windows-1250
        raw.extend_from_slice(b"</title></html>");

        assert_eq!(
            decode(&raw, None),
            "<html><head><meta charset=\"windows-1250\"><title>ł</title></html>"
        );
    }

    #[test]
    fn the_http_equiv_form_declares_an_encoding_too() {
        let mut raw =
            b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1250\"><t>"
                .to_vec();
        raw.extend_from_slice(WINDOWS_1250_GREETING);

        assert!(decode(&raw, None).contains('ł'), "{}", decode(&raw, None));
    }

    #[test]
    fn iso_8859_2_is_honoured_as_its_own_encoding() {
        let mut raw = b"<meta charset=\"ISO-8859-2\"><title>".to_vec();
        raw.extend_from_slice(&[0xB3]);
        raw.extend_from_slice(b"</title>");

        assert_eq!(
            decode(&raw, None),
            "<meta charset=\"ISO-8859-2\"><title>ł</title>"
        );
    }

    #[test]
    fn a_byte_order_mark_overrides_a_lie_in_the_meta_tag() {
        let marked = "\u{FEFF}<title>żółć</title>";

        assert_eq!(decode(marked.as_bytes(), None), "<title>żółć</title>");
    }

    #[test]
    fn a_charset_outside_the_window_is_never_seen() {
        let mut raw = b"<html><head><title>x</title>".to_vec();
        raw.extend(vec![b' '; DECLARATION_WINDOW_BYTES]);
        raw.extend_from_slice(b"<meta charset=\"windows-1250\">");

        // Decoded as UTF-8, the declaration arrives too late to matter and
        // the text survives as whatever it was.
        assert!(decode(&raw, None).starts_with("<html><head><title>x</title>"));
    }

    #[test]
    fn an_unnameable_charset_falls_back_to_utf8_rather_than_failing() {
        let raw = "<meta charset=\"x-mac-roman-cyrillic\"><title>ok</title>";

        assert_eq!(
            decode(raw.as_bytes(), None),
            "<meta charset=\"x-mac-roman-cyrillic\"><title>ok</title>"
        );
    }

    #[test]
    fn undecodable_bytes_become_replacement_characters_instead_of_an_error() {
        // 0xC3 promises a two-byte UTF-8 sequence the `(` cannot finish: the
        // promise becomes a replacement character, the innocent byte survives.
        assert_eq!(decode(&[0xC3, b'('], None), "\u{FFFD}(");
    }

    #[test]
    fn a_quoted_header_value_is_read_without_its_quotes() {
        assert!(from_content_type("text/html; charset=\"iso-8859-2\"").is_some());
    }

    #[test]
    fn a_header_without_a_charset_names_nothing() {
        assert_eq!(from_content_type("text/html"), None);
        assert_eq!(from_content_type("application/json"), None);
    }

    #[test]
    fn a_charset_attribute_is_not_confused_with_a_longer_name() {
        // `data-charset=` ends with the searched name but is a different
        // attribute; the declaration after it is the one that counts.
        let mut raw =
            b"<div data-charset=\"utf-16\"><meta charset=\"windows-1250\"><title>".to_vec();
        raw.extend_from_slice(&[0xB3]);
        raw.extend_from_slice(b"</title>");

        assert_eq!(
            decode(&raw, None),
            "<div data-charset=\"utf-16\"><meta charset=\"windows-1250\"><title>ł</title>"
        );
    }
}
