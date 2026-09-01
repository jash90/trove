//! Reading a page's title, icons and nominated picture out of its markup.
//!
//! Deliberately not a parser. This looks for a handful of things in a bounded
//! prefix of the document and stops; a clipboard preview does not need to know
//! what the page means, and a full parser would be a much larger thing to trust
//! with somebody else's bytes.
//!
//! What is collected follows the priority chain real unfurlers use: an oEmbed
//! discovery link first (it is the source of truth for video and music
//! services), then the Open Graph family, then Twitter cards, then whatever
//! JSON-LD says. This module only reads; which source wins is decided where
//! things are fetched.

/// Longest title kept. Anything past this is a page abusing the field.
const MAX_TITLE_CHARS: usize = 200;

/// What the markup offered.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PageMetadata {
    pub title: Option<String>,
    /// The icon reference exactly as the page wrote it, still relative.
    ///
    /// A plain favicon is a 16-pixel mark. Kept separate from the touch icon
    /// because they answer different questions and are worth different sizes.
    pub icon_href: Option<String>,
    /// The large square icon pages ship for home-screen bookmarks.
    ///
    /// When a page offers one it beats every favicon: it is drawn to be seen
    /// at a size a preview actually shows.
    pub apple_touch_icon_href: Option<String>,
    /// The `<link rel="alternate">` pointing at an oEmbed endpoint.
    ///
    /// Services that host media describe themselves there better than their
    /// Open Graph tags do — a thumbnail picked for the actual video rather
    /// than whatever card the page template renders.
    pub oembed_href: Option<String>,
    /// Where relative references resolve from, when the page moved the base.
    ///
    /// A `<base href>` rewrites every relative path on the page, pictures
    /// included. Ignoring it resolves icons and cards against the wrong place,
    /// and the fetch then fails quietly for reasons no error message names.
    pub base_href: Option<String>,
    /// Pictures the page nominates for itself, best first, still relative.
    ///
    /// More than one: `og:image` is an ordered list, not a value, and a page
    /// may offer several. Each carries the dimensions the page declared next
    /// to it, when it did — they belong to the picture they follow, which is
    /// why they are attached while walking rather than gathered separately.
    pub image_candidates: Vec<ImageCandidate>,
    /// The web app manifest a page declares, if it declares one.
    ///
    /// Only read when the markup named no icon: the manifest lists icons
    /// drawn for a home screen, but reaching it costs a request the page did
    /// not otherwise need, and most pages name an icon outright.
    pub manifest_href: Option<String>,
    /// Facebook's nomination from before Open Graph existed.
    ///
    /// Last in the order and still worth reading: a page old enough to emit
    /// only this one has nothing else to offer, and the alternative for it is
    /// no picture at all.
    pub image_src_href: Option<String>,
    /// Twitter's nomination, when the page wrote one.
    ///
    /// Kept apart from the Open Graph list: the sources are consulted in
    /// order, and a page that predates Open Graph often fills only this one.
    pub twitter_image_href: Option<String>,
}

/// One nominated picture, with what the page said about it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageCandidate {
    pub href: String,
    pub declared_width: Option<u32>,
    pub declared_height: Option<u32>,
}

impl ImageCandidate {
    fn from_href(href: String) -> Self {
        Self {
            href,
            declared_width: None,
            declared_height: None,
        }
    }
}

/// Pulls everything worth knowing out of a document.
pub fn read_metadata(document: &str) -> PageMetadata {
    let lowered = document.to_ascii_lowercase();
    let mut metadata = PageMetadata::default();

    for (tag, lowered_tag) in tags(document, &lowered, "<meta") {
        let Some(declared) = read_attribute(tag, lowered_tag, "property")
            .or_else(|| read_attribute(tag, lowered_tag, "name"))
        else {
            continue;
        };
        // Both spellings exist in the wild; canonicalising once keeps every
        // later comparison a plain match.
        let property = declared.trim().to_ascii_lowercase();
        if property != "twitter:image"
            && property != "twitter:image:src"
            && !property.starts_with("og:image")
        {
            continue;
        }
        let Some(content) = read_attribute(tag, lowered_tag, "content")
            .map(|content| decode_entities(content.trim()))
            .filter(|content| !content.is_empty())
        else {
            continue;
        };
        record_meta(&mut metadata, &property, content);
    }

    for (tag, lowered_tag) in tags(document, &lowered, "<link") {
        read_link(&mut metadata, tag, lowered_tag);
    }

    for (tag, lowered_tag) in tags(document, &lowered, "<base") {
        if let Some(href) = read_attribute(tag, lowered_tag, "href") {
            let href = href.trim();
            if !href.is_empty() {
                // The first declaration wins, as in a browser.
                metadata.base_href = Some(decode_entities(href));
                break;
            }
        }
    }

    metadata.title = read_title(document).map(|title| collapse_whitespace(&title));
    metadata
}

/// Files one meta tag's content under the candidate it belongs to.
///
/// `og:image` opens a candidate; `og:image:width` and friends attach to the
/// one before them. The `url` and `secure_url` variants refine that same
/// candidate rather than opening another — and where a page declares both a
/// plain and a secure address, the secure one wins, because a preview fetched
/// over plain HTTP from an HTTPS page would never render at all.
fn record_meta(metadata: &mut PageMetadata, property: &str, content: String) {
    match property {
        "og:image" => metadata
            .image_candidates
            .push(ImageCandidate::from_href(content)),
        "og:image:url" | "og:image:secure_url" => match metadata.image_candidates.last_mut() {
            Some(candidate) => {
                let replaces =
                    content.starts_with("https://") && !candidate.href.starts_with("https://");
                if replaces || candidate.href.is_empty() {
                    candidate.href = content;
                }
            }
            // A page may write the structured variant without ever writing
            // the plain tag. It is still a nomination, just a bare one.
            None => metadata
                .image_candidates
                .push(ImageCandidate::from_href(content)),
        },
        "og:image:width" | "og:image:height" => {
            let Some(candidate) = metadata.image_candidates.last_mut() else {
                return;
            };
            let parsed = content.trim().parse::<u32>().ok();
            if property.ends_with("width") {
                candidate.declared_width = parsed;
            } else {
                candidate.declared_height = parsed;
            }
        }
        // The old spelling survives on pages that have not been touched in
        // years — which are exactly the pages with nothing else to offer.
        "twitter:image" | "twitter:image:src" => {
            metadata.twitter_image_href.get_or_insert(content);
        }
        _ => {}
    }
}

/// Reads one `<link>`: an icon, a touch icon, a nominated picture, or an
/// oEmbed discovery link.
fn read_link(metadata: &mut PageMetadata, tag: &str, lowered_tag: &str) {
    let Some(rel) = read_attribute(tag, lowered_tag, "rel") else {
        return;
    };
    let words = rel.split_ascii_whitespace().collect::<Vec<_>>();
    if words
        .iter()
        .any(|word| word.eq_ignore_ascii_case("alternate"))
        && read_attribute(tag, lowered_tag, "type")
            .is_some_and(|kind| kind.contains("application/json+oembed"))
    {
        if let Some(href) = read_attribute(tag, lowered_tag, "href") {
            let href = href.trim();
            if !href.is_empty() {
                metadata
                    .oembed_href
                    .get_or_insert_with(|| decode_entities(href));
            }
        }
        return;
    }
    if words.iter().any(|word| {
        word.eq_ignore_ascii_case("apple-touch-icon")
            || word.eq_ignore_ascii_case("apple-touch-icon-precomposed")
    }) {
        if let Some(href) = read_attribute(tag, lowered_tag, "href") {
            let href = href.trim();
            if !href.is_empty() {
                metadata
                    .apple_touch_icon_href
                    .get_or_insert_with(|| decode_entities(href));
            }
        }
        return;
    }
    if words
        .iter()
        .any(|word| word.eq_ignore_ascii_case("manifest"))
    {
        if let Some(href) = read_attribute(tag, lowered_tag, "href") {
            let href = href.trim();
            if !href.is_empty() {
                metadata
                    .manifest_href
                    .get_or_insert_with(|| decode_entities(href));
            }
        }
        return;
    }
    // Checked before the icon rule below rather than after: `image_src` is a
    // picture, not a mark, and the two live in different slots.
    if words
        .iter()
        .any(|word| word.eq_ignore_ascii_case("image_src"))
    {
        if let Some(href) = read_attribute(tag, lowered_tag, "href") {
            let href = href.trim();
            if !href.is_empty() {
                metadata
                    .image_src_href
                    .get_or_insert_with(|| decode_entities(href));
            }
        }
        return;
    }
    // `shortcut icon` spells the same thing in two words, so either word
    // alone counts; anything longer (`apple-touch-icon`) does not.
    let is_icon = words
        .iter()
        .any(|word| word.eq_ignore_ascii_case("icon") || word.eq_ignore_ascii_case("shortcut"));
    if is_icon && let Some(href) = read_attribute(tag, lowered_tag, "href") {
        let href = href.trim();
        if !href.is_empty() {
            metadata
                .icon_href
                .get_or_insert_with(|| decode_entities(href));
        }
    }
}

/// Walks every occurrence of one tag through a bounded document prefix.
///
/// Yields the tag as written and lower-cased, so attribute readers can match
/// names without re-lowering per call. An unterminated tag ends the walk: the
/// markup past it is broken beyond what a preview should care about.
fn tags<'a>(
    document: &'a str,
    lowered: &'a str,
    open: &str,
) -> impl Iterator<Item = (&'a str, &'a str)> {
    let mut cursor = 0_usize;
    std::iter::from_fn(move || {
        let offset = lowered[cursor..].find(open)?;
        let start = cursor + offset;
        let length = lowered[start..].find('>')?;
        let tag = &document[start..start + length];
        let lowered_tag = &lowered[start..start + length];
        cursor = start + length + 1;
        Some((tag, lowered_tag))
    })
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
    fn a_legacy_image_src_link_is_read() {
        // Facebook's pre-Open-Graph nomination. Older content systems still
        // emit it and nothing else, so for those pages it is the only picture
        // on offer.
        for document in [
            r#"<link rel="image_src" href="/assets/card.png">"#,
            r#"<link href='/assets/card.png' rel='image_src'>"#,
            r#"<link REL=IMAGE_SRC HREF=/assets/card.png>"#,
        ] {
            assert_eq!(
                read_metadata(document).image_src_href.as_deref(),
                Some("/assets/card.png"),
                "{document}"
            );
        }
    }

    #[test]
    fn image_src_is_not_mistaken_for_an_icon() {
        let metadata = read_metadata(r#"<link rel="image_src" href="/assets/card.png">"#);

        assert_eq!(metadata.icon_href, None);
        assert_eq!(metadata.apple_touch_icon_href, None);
    }

    #[test]
    fn a_declared_icon_is_preferred_over_guessing() {
        for document in [
            r#"<link rel="icon" href="/assets/icon.png">"#,
            r#"<link href='/assets/icon.png' rel='shortcut icon'>"#,
            r#"<link rel=icon href=/assets/icon.png>"#,
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
    fn a_touch_icon_is_kept_apart_from_the_favicon() {
        let document = concat!(
            r#"<link rel="apple-touch-icon" href="/touch.png">"#,
            r#"<link rel="icon" href="/favicon.png">"#,
        );
        let metadata = read_metadata(document);

        assert_eq!(
            metadata.apple_touch_icon_href.as_deref(),
            Some("/touch.png")
        );
        assert_eq!(metadata.icon_href.as_deref(), Some("/favicon.png"));
    }

    #[test]
    fn a_precomposed_touch_icon_still_counts_as_one() {
        let document = r#"<link rel="apple-touch-icon-precomposed" href="/touch.png">"#;

        assert_eq!(
            read_metadata(document).apple_touch_icon_href.as_deref(),
            Some("/touch.png")
        );
    }

    #[test]
    fn the_first_of_each_icon_kind_wins() {
        let document = concat!(
            r#"<link rel="icon" href="/first.png">"#,
            r#"<link rel="icon" href="/second.png">"#,
        );

        assert_eq!(
            read_metadata(document).icon_href.as_deref(),
            Some("/first.png")
        );
    }

    #[test]
    fn an_oembed_discovery_link_is_read_wherever_its_attributes_sit() {
        for document in [
            r#"<link rel="alternate" type="application/json+oembed" href="https://example.invalid/oembed?url=x">"#,
            r#"<link type='application/json+oembed' href='https://example.invalid/oembed?url=x' rel='alternate'>"#,
        ] {
            assert_eq!(
                read_metadata(document).oembed_href.as_deref(),
                Some("https://example.invalid/oembed?url=x"),
                "{document}"
            );
        }
    }

    #[test]
    fn an_xml_oembed_link_is_left_for_a_parser_that_does_not_exist() {
        // Only the JSON flavour is followed; the XML one would need a second
        // parser for endpoints that mostly mirror the JSON anyway.
        let document = r#"<link rel="alternate" type="application/xml+oembed" href="/oembed.xml">"#;

        assert_eq!(read_metadata(document).oembed_href, None);
    }

    #[test]
    fn a_plain_alternate_link_is_not_an_oembed_endpoint() {
        let document = r#"<link rel="alternate" type="application/rss+xml" href="/feed.xml">"#;

        assert_eq!(read_metadata(document).oembed_href, None);
    }

    #[test]
    fn a_base_declaration_moves_where_relative_paths_resolve_from() {
        let document = r#"<html><head><base href="https://cdn.example.invalid/v2/"></head></html>"#;

        assert_eq!(
            read_metadata(document).base_href.as_deref(),
            Some("https://cdn.example.invalid/v2/")
        );
    }

    #[test]
    fn the_picture_a_page_nominates_is_read() {
        for document in [
            r#"<meta property="og:image" content="https://example.invalid/card.png">"#,
            r#"<meta content="https://example.invalid/card.png" property="og:image">"#,
            // Written with `name` instead of `property`, which many pages do.
            r#"<meta name="og:image" content="https://example.invalid/card.png">"#,
        ] {
            assert_eq!(
                read_metadata(document)
                    .image_candidates
                    .first()
                    .map(|c| c.href.clone()),
                Some("https://example.invalid/card.png".to_owned()),
                "{document}"
            );
        }
    }

    #[test]
    fn several_nominated_pictures_are_kept_in_the_order_they_were_written() {
        let document = concat!(
            r#"<meta property="og:image" content="https://example.invalid/a.jpg">"#,
            r#"<meta property="og:image" content="https://example.invalid/b.jpg">"#,
            r#"<meta property="og:image" content="https://example.invalid/c.jpg">"#,
        );
        let candidates = read_metadata(document).image_candidates;

        let hrefs: Vec<_> = candidates.iter().map(|c| c.href.as_str()).collect();
        assert_eq!(
            hrefs,
            [
                "https://example.invalid/a.jpg",
                "https://example.invalid/b.jpg",
                "https://example.invalid/c.jpg"
            ]
        );
    }

    #[test]
    fn declared_dimensions_attach_to_the_picture_they_follow() {
        // The exact shape the specification warns about: dimensions written
        // after each picture belong to that picture, not to the first one.
        let document = concat!(
            r#"<meta property="og:image" content="https://example.invalid/a.jpg">"#,
            r#"<meta property="og:image:width" content="300">"#,
            r#"<meta property="og:image:height" content="200">"#,
            r#"<meta property="og:image" content="https://example.invalid/b.jpg">"#,
            r#"<meta property="og:image:width" content="1200">"#,
        );
        let candidates = read_metadata(document).image_candidates;

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].declared_width, Some(300));
        assert_eq!(candidates[0].declared_height, Some(200));
        assert_eq!(candidates[1].declared_width, Some(1200));
        assert_eq!(candidates[1].declared_height, None);
    }

    #[test]
    fn dimensions_before_any_picture_are_attached_to_nothing() {
        let document = concat!(
            r#"<meta property="og:image:width" content="300">"#,
            r#"<meta property="og:image" content="https://example.invalid/a.jpg">"#,
        );
        let candidates = read_metadata(document).image_candidates;

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].declared_width, None);
    }

    #[test]
    fn a_dimension_that_is_not_a_number_is_ignored_rather_than_fatal() {
        let document = concat!(
            r#"<meta property="og:image" content="https://example.invalid/a.jpg">"#,
            r#"<meta property="og:image:width" content="auto">"#,
        );

        assert_eq!(
            read_metadata(document).image_candidates[0].declared_width,
            None
        );
    }

    #[test]
    fn the_secure_variant_replaces_a_plain_address_on_the_same_picture() {
        let document = concat!(
            r#"<meta property="og:image" content="http://example.invalid/plain.png">"#,
            r#"<meta property="og:image:secure_url" content="https://example.invalid/secure.png">"#,
        );
        let candidates = read_metadata(document).image_candidates;

        // One picture, carrying its secure address — not two candidates.
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].href, "https://example.invalid/secure.png");
    }

    #[test]
    fn a_secure_address_already_recorded_is_not_demoted_by_a_plain_one() {
        let document = concat!(
            r#"<meta property="og:image:secure_url" content="https://example.invalid/secure.png">"#,
            r#"<meta property="og:image:url" content="http://example.invalid/plain.png">"#,
        );
        let candidates = read_metadata(document).image_candidates;

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].href, "https://example.invalid/secure.png");
    }

    #[test]
    fn a_structured_variant_without_a_plain_tag_still_nominates_a_picture() {
        let document =
            r#"<meta property="og:image:secure_url" content="https://example.invalid/secure.png">"#;

        assert_eq!(
            read_metadata(document)
                .image_candidates
                .first()
                .map(|c| c.href.clone()),
            Some("https://example.invalid/secure.png".to_owned())
        );
    }

    #[test]
    fn the_twitter_nomination_is_kept_separate_from_open_graphs() {
        let document =
            r#"<meta name="twitter:image" content="https://example.invalid/twitter.png">"#;

        assert_eq!(
            read_metadata(document).twitter_image_href.as_deref(),
            Some("https://example.invalid/twitter.png")
        );
        assert!(read_metadata(document).image_candidates.is_empty());
    }

    #[test]
    fn the_old_twitter_spelling_counts_as_the_same_nomination() {
        let document = concat!(
            r#"<meta name="twitter:image" content="https://example.invalid/new.png">"#,
            r#"<meta name="twitter:image:src" content="https://example.invalid/old.png">"#,
        );

        assert_eq!(
            read_metadata(document).twitter_image_href.as_deref(),
            Some("https://example.invalid/new.png")
        );
    }

    #[test]
    fn an_unrelated_meta_tag_is_not_mistaken_for_the_picture() {
        let document = concat!(
            r#"<meta charset="utf-8">"#,
            r#"<meta name="description" content="not a picture">"#,
            r#"<meta property="og:title" content="also not a picture">"#,
        );

        assert!(read_metadata(document).image_candidates.is_empty());
    }

    #[test]
    fn a_meta_tag_with_no_name_does_not_end_the_search() {
        // Every real document opens with `<meta charset>`, which names neither
        // `property` nor `name`. Stopping there found nothing on any page in
        // the world, while the tests still passed.
        let document = concat!(
            r#"<meta charset="utf-8">"#,
            r#"<meta http-equiv="X-UA-Compatible" content="IE=edge">"#,
            r#"<meta property="og:image" content="https://example.invalid/card.png">"#,
        );

        assert_eq!(
            read_metadata(document)
                .image_candidates
                .first()
                .map(|c| c.href.clone()),
            Some("https://example.invalid/card.png".to_owned())
        );
    }

    #[test]
    fn an_empty_content_is_not_a_nomination() {
        let document = r#"<meta property="og:image" content="">"#;

        assert!(read_metadata(document).image_candidates.is_empty());
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

    #[test]
    fn whitespace_in_a_title_collapses_but_is_preserved_everywhere_else() {
        let document = r#"<meta property="og:image" content="  https://example.invalid/a.jpg  ">"#;

        // Paths may legally contain inner spaces; trimming belongs to the
        // edges only.
        assert_eq!(
            read_metadata(document)
                .image_candidates
                .first()
                .map(|c| c.href.clone()),
            Some("https://example.invalid/a.jpg".to_owned())
        );
        assert_eq!(collapse_whitespace("a  b\tc"), "a b c");
    }
}
