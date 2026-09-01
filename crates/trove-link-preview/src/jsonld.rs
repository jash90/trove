//! What a page says about itself in machine-readable form.
//!
//! Structured data is where a page keeps the facts it wants a search engine
//! to know, and among them is usually a picture chosen more deliberately than
//! the social card. It arrives in shapes that vary by site, so what this reads
//! has to be normalised rather than parsed once.
//!
//! Only `image` and `thumbnailUrl` are followed — an Article's photograph, a
//! Product's shot, a VideoObject's frame. Everything else in the graph
//! describes things no preview shows.

use serde_json::Value;

/// How many hrefs one document may contribute.
///
/// A page with a deep graph can name dozens of pictures; only the first few
/// would ever be tried, and bounding here bounds everything downstream too.
const MAX_HREFS: usize = 8;

/// How deeply nested structures are followed before giving up.
///
/// Real graphs nest three or four levels; the bound exists for hostile input,
/// not for real pages.
const MAX_DEPTH: usize = 8;

/// Every picture the document's structured data names, best first.
pub fn image_hrefs(document: &str) -> Vec<String> {
    let mut hrefs = Vec::new();
    for block in script_blocks(document) {
        let Ok(value) = serde_json::from_str::<Value>(block) else {
            // A block that does not parse is skipped, not fatal: pages carry
            // several, and one broken template must not hide the rest.
            continue;
        };
        collect(&value, 0, &mut hrefs);
        if hrefs.len() >= MAX_HREFS {
            break;
        }
    }
    hrefs.truncate(MAX_HREFS);
    hrefs
}

/// Finds `<script type="application/ld+json">` blocks and returns their text.
fn script_blocks(document: &str) -> Vec<&str> {
    let lowered = document.to_ascii_lowercase();
    let mut blocks = Vec::new();
    let mut cursor = 0_usize;
    while let Some(offset) = lowered[cursor..].find("<script") {
        let start = cursor + offset;
        let Some(open_close) = lowered[start..].find('>') else {
            break;
        };
        let attributes = &lowered[start..start + open_close];
        let body_start = start + open_close + 1;
        if attributes.contains("application/ld+json") {
            match lowered[body_start..].find("</script>") {
                Some(body_len) => {
                    blocks.push(&document[body_start..body_start + body_len]);
                    cursor = body_start + body_len;
                }
                // An unterminated script swallows the rest of the document;
                // nothing after it can be trusted as markup anyway.
                None => break,
            }
        } else {
            cursor = body_start;
        }
    }
    blocks
}

/// Walks one JSON value, gathering every picture address it names.
fn collect(value: &Value, depth: usize, hrefs: &mut Vec<String>) {
    if hrefs.len() >= MAX_HREFS || depth > MAX_DEPTH {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let key = key.to_ascii_lowercase();
                if key == "image" || key == "thumbnailurl" || key == "thumbnail" {
                    gather(child, depth + 1, hrefs);
                } else if child.is_object() || child.is_array() {
                    // `@graph` and nested entities carry their own images
                    // further down; anything scalar under another name is not
                    // a picture and is not descended into.
                    collect(child, depth + 1, hrefs);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(item, depth + 1, hrefs);
            }
        }
        _ => {}
    }
}

/// Reads one `image` value in whichever of its shapes the page used.
///
/// The specification allows it to be a string, an array of strings, an object
/// with a `url`, or an array of those objects. All of them occur in the wild,
/// sometimes on the same page.
fn gather(value: &Value, depth: usize, hrefs: &mut Vec<String>) {
    if hrefs.len() >= MAX_HREFS || depth > MAX_DEPTH {
        return;
    }
    match value {
        Value::String(text) => push(hrefs, text),
        Value::Array(items) => items.iter().for_each(|item| gather(item, depth + 1, hrefs)),
        Value::Object(map) => {
            if let Some(url) = map.get("url").or_else(|| map.get("contentUrl")) {
                gather(url, depth + 1, hrefs);
            }
        }
        _ => {}
    }
}

fn push(hrefs: &mut Vec<String>, text: &str) {
    let trimmed = text.trim();
    if !trimmed.is_empty() && !hrefs.iter().any(|seen| seen == trimmed) {
        hrefs.push(trimmed.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(json: &str) -> String {
        format!(r#"<script type="application/ld+json">{json}</script>"#)
    }

    #[test]
    fn a_plain_string_image_is_read() {
        let document = block(r#"{"@type":"Article","image":"https://cdn.invalid/a.jpg"}"#);

        assert_eq!(image_hrefs(&document), ["https://cdn.invalid/a.jpg"]);
    }

    #[test]
    fn an_array_of_strings_is_read_in_order() {
        let document =
            block(r#"{"image":["https://cdn.invalid/a.jpg","https://cdn.invalid/b.jpg"]}"#);

        assert_eq!(
            image_hrefs(&document),
            ["https://cdn.invalid/a.jpg", "https://cdn.invalid/b.jpg"]
        );
    }

    #[test]
    fn an_image_object_offers_its_url() {
        let document = block(
            r#"{"image":{"@type":"ImageObject","url":"https://cdn.invalid/a.jpg","width":1200}}"#,
        );

        assert_eq!(image_hrefs(&document), ["https://cdn.invalid/a.jpg"]);
    }

    #[test]
    fn content_url_serves_where_url_is_absent() {
        let document = block(
            r#"{"thumbnail":{"@type":"ImageObject","contentUrl":"https://cdn.invalid/t.jpg"}}"#,
        );

        assert_eq!(image_hrefs(&document), ["https://cdn.invalid/t.jpg"]);
    }

    #[test]
    fn an_array_of_objects_is_read_item_by_item() {
        let document = block(
            r#"{"image":[{"url":"https://cdn.invalid/a.jpg"},{"url":"https://cdn.invalid/b.jpg"}]}"#,
        );

        assert_eq!(
            image_hrefs(&document),
            ["https://cdn.invalid/a.jpg", "https://cdn.invalid/b.jpg"]
        );
    }

    #[test]
    fn a_graph_of_entities_contributes_each_one_images() {
        let document = block(
            r#"{"@graph":[
                 {"@type":"Article","image":"https://cdn.invalid/article.jpg"},
                 {"@type":"VideoObject","thumbnailUrl":"https://cdn.invalid/frame.jpg"}
               ]}"#,
        );

        assert_eq!(
            image_hrefs(&document),
            [
                "https://cdn.invalid/article.jpg",
                "https://cdn.invalid/frame.jpg"
            ]
        );
    }

    #[test]
    fn thumbnail_url_on_a_video_counts_as_a_picture() {
        let document =
            block(r#"{"@type":"VideoObject","thumbnailUrl":"https://cdn.invalid/frame.jpg"}"#);

        assert_eq!(image_hrefs(&document), ["https://cdn.invalid/frame.jpg"]);
    }

    #[test]
    fn unrelated_fields_are_not_descended_into_for_scalars() {
        let document = block(r#"{"name":"A Photo of image","author":"image"}"#);

        assert!(image_hrefs(&document).is_empty());
    }

    #[test]
    fn several_blocks_are_read_and_a_broken_one_skipped() {
        let good = block(r#"{"image":"https://cdn.invalid/a.jpg"}"#);
        let broken = r#"<script type="application/ld+json">{"image": </script>"#;

        assert_eq!(
            image_hrefs(&format!("{broken}{good}")),
            ["https://cdn.invalid/a.jpg"]
        );
    }

    #[test]
    fn duplicates_are_collapsed() {
        let document =
            block(r#"{"image":["https://cdn.invalid/a.jpg",["https://cdn.invalid/a.jpg"]]}"#);

        assert_eq!(image_hrefs(&document), ["https://cdn.invalid/a.jpg"]);
    }

    #[test]
    fn a_document_carrying_no_structured_data_yields_nothing() {
        assert!(image_hrefs("<html><body>plain</body></html>").is_empty());
        assert!(
            image_hrefs(r#"<script type="text/javascript">var image = "not json";</script>"#)
                .is_empty()
        );
    }

    #[test]
    fn a_href_cap_bounds_a_hostile_graph() {
        let many = (0..64)
            .map(|index| format!(r#""https://cdn.invalid/{index}.jpg""#))
            .collect::<Vec<_>>()
            .join(",");
        let document = block(&format!(r#"{{"image":[{many}]}}"#));

        assert_eq!(image_hrefs(&document).len(), MAX_HREFS);
    }

    #[test]
    fn an_unterminated_script_ends_the_scan_rather_than_running_away() {
        let document = r#"<script type="application/ld+json">{"image":"never closed"#;

        assert!(image_hrefs(document).is_empty());
    }
}
