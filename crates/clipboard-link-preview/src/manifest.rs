//! Icons a page lists in its web app manifest.
//!
//! The manifest is where a site keeps the icons it means for a home screen —
//! drawn to be seen at size, unlike the sixteen-pixel mark in `favicon.ico`.
//! It is consulted only when the markup offered no icon at all, because
//! reading it costs a request the page did not otherwise need.

use serde_json::Value;

/// How many icons one manifest may contribute.
///
/// Only the first few would ever be tried; a manifest listing forty is either
/// unusual or hostile, and neither deserves the walk.
const MAX_ICONS: usize = 6;

/// Every icon the manifest names, largest first.
///
/// Ordering is the whole point of reading this rather than taking the first
/// entry: manifests conventionally list smallest first, which is the opposite
/// of what a preview wants.
pub fn icon_hrefs(document: &str) -> Vec<String> {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(document) else {
        return Vec::new();
    };
    let Some(Value::Array(icons)) = root.get("icons") else {
        return Vec::new();
    };
    let mut ranked = icons
        .iter()
        .filter_map(|icon| {
            let source = icon.get("src")?.as_str()?.trim();
            if source.is_empty() {
                return None;
            }
            let area = icon
                .get("sizes")
                .and_then(Value::as_str)
                .map_or(0, largest_area);
            Some((area, source.to_owned()))
        })
        .collect::<Vec<_>>();
    // Stable, so that among equal-sized entries the manifest's own order wins
    // rather than something arbitrary.
    ranked.sort_by_key(|(area, _)| std::cmp::Reverse(*area));
    ranked
        .into_iter()
        .map(|(_, source)| source)
        .take(MAX_ICONS)
        .collect()
}

/// The largest area named in a `sizes` attribute.
///
/// `sizes` holds a list — `"48x48 96x96 192x192"` — and the entry is worth
/// whatever its biggest member is. `"any"` names a scalable icon and no size
/// at all; it sorts last rather than first, because an SVG that happens to be
/// a monochrome glyph is a worse preview than a real 192-pixel picture.
fn largest_area(sizes: &str) -> u64 {
    sizes
        .split_ascii_whitespace()
        .filter_map(|pair| {
            let (width, height) = pair.split_once(['x', 'X'])?;
            Some(u64::from(width.parse::<u32>().ok()?) * u64::from(height.parse::<u32>().ok()?))
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_come_back_largest_first() {
        let manifest = r#"{"icons":[
            {"src":"/i/48.png","sizes":"48x48"},
            {"src":"/i/192.png","sizes":"192x192"},
            {"src":"/i/96.png","sizes":"96x96"}
        ]}"#;

        assert_eq!(
            icon_hrefs(manifest),
            vec!["/i/192.png", "/i/96.png", "/i/48.png"]
        );
    }

    #[test]
    fn a_scalable_entry_does_not_outrank_a_real_size() {
        // "any" means no size, and sorting it first would put a monochrome
        // glyph ahead of a picture drawn to be looked at.
        let manifest = r#"{"icons":[
            {"src":"/i/any.svg","sizes":"any"},
            {"src":"/i/192.png","sizes":"192x192"}
        ]}"#;

        assert_eq!(icon_hrefs(manifest), vec!["/i/192.png", "/i/any.svg"]);
    }

    #[test]
    fn a_multi_size_entry_is_worth_its_largest() {
        let manifest = r#"{"icons":[
            {"src":"/i/one.png","sizes":"128x128"},
            {"src":"/i/many.ico","sizes":"16x16 32x32 256x256"}
        ]}"#;

        assert_eq!(icon_hrefs(manifest), vec!["/i/many.ico", "/i/one.png"]);
    }

    #[test]
    fn entries_without_a_source_are_skipped_rather_than_ranked() {
        let manifest = r#"{"icons":[
            {"sizes":"512x512"},
            {"src":"   ","sizes":"512x512"},
            {"src":"/i/real.png","sizes":"64x64"}
        ]}"#;

        assert_eq!(icon_hrefs(manifest), vec!["/i/real.png"]);
    }

    #[test]
    fn anything_that_is_not_a_manifest_contributes_nothing() {
        for document in [
            "",
            "not json at all",
            "[]",
            r#"{"icons":"not an array"}"#,
            r#"{"name":"no icons here"}"#,
        ] {
            assert!(icon_hrefs(document).is_empty(), "{document}");
        }
    }

    #[test]
    fn a_manifest_listing_many_icons_is_bounded() {
        let icons = (0..40)
            .map(|index| format!(r#"{{"src":"/i/{index}.png","sizes":"{index}x{index}"}}"#))
            .collect::<Vec<_>>()
            .join(",");

        assert_eq!(
            icon_hrefs(&format!("{{\"icons\":[{icons}]}}")).len(),
            MAX_ICONS
        );
    }
}
