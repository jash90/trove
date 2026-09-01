//! Tests for everything a fetch means, run without a network.
//!
//! The pipeline sits on a [`Transport`] trait precisely so these tests can
//! stand in for the wire: routes are declared per address, and every decision
//! above the socket — which source wins, which picture is refused, which hop
//! is denied — is then exercised end to end.

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

/// A transport that answers from a table.
struct TestTransport {
    routes: HashMap<String, WireResponse>,
    /// Every request, as (address, user agent), in order.
    seen: Mutex<Vec<(String, &'static str)>>,
}

impl TestTransport {
    fn with(routes: Vec<(&str, WireResponse)>) -> Self {
        Self {
            routes: routes
                .into_iter()
                .map(|(url, response)| (url.to_owned(), response))
                .collect(),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn requests_for(&self, url: &str) -> Vec<&'static str> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen_url, _)| seen_url == url)
            .map(|(_, agent)| *agent)
            .collect()
    }
}

impl Transport for TestTransport {
    async fn send(
        &self,
        url: &url::Url,
        user_agent: &'static str,
        _max_body: usize,
    ) -> Result<WireResponse, LinkPreviewError> {
        self.seen
            .lock()
            .unwrap()
            .push((url.to_string(), user_agent));
        match self.routes.get(url.as_str()) {
            Some(response) => Ok(response.clone()),
            None => Err(LinkPreviewError::Unreachable),
        }
    }
}

fn page(status: u16, body: &str) -> WireResponse {
    WireResponse {
        status,
        content_type: Some("text/html".to_owned()),
        location: None,
        body: body.as_bytes().to_vec(),
    }
}

fn image(status: u16, mime: &str, bytes: Vec<u8>) -> WireResponse {
    WireResponse {
        status,
        content_type: Some(mime.to_owned()),
        location: None,
        body: bytes,
    }
}

fn redirect(status: u16, location: &str) -> WireResponse {
    WireResponse {
        status,
        content_type: None,
        location: Some(location.to_owned()),
        body: Vec::new(),
    }
}

fn png_bytes(width: u32, height: u32) -> Vec<u8> {
    let buffer = image::RgbaImage::from_pixel(width, height, image::Rgba([90, 120, 150, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}

fn jpeg_bytes(width: u32, height: u32) -> Vec<u8> {
    let buffer = image::RgbImage::from_pixel(width, height, image::Rgb([200, 100, 50]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(buffer)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    bytes
}

fn og_page(image_url: &str) -> String {
    format!(
        "<html><head><title>Synthetic Page</title>\
         <meta property=\"og:image\" content=\"{image_url}\"></head><body></body></html>"
    )
}

#[tokio::test]
async fn a_page_yields_its_title_icon_and_picture() {
    let transport = TestTransport::with(vec![
        (
            "https://example.invalid/",
            page(
                200,
                &format!(
                    "<html><head><title>Synthetic Page</title>\
                     <link rel=\"icon\" href=\"/mark.png\">{}</head></html>",
                    og_page("/card.png")
                ),
            ),
        ),
        (
            "https://example.invalid/mark.png",
            image(200, "image/png", png_bytes(32, 32)),
        ),
        (
            "https://example.invalid/card.png",
            image(200, "image/png", png_bytes(640, 360)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://example.invalid/")
        .await
        .unwrap();

    assert_eq!(preview.title.as_deref(), Some("Synthetic Page"));
    assert_eq!(preview.icon_mime.as_deref(), Some("image/png"));
    assert_eq!(preview.image_mime.as_deref(), Some("image/png"));
    // The card was kept at its fetched size; shrinking happens above this crate.
    assert_eq!(dimension_of(preview.image.as_deref().unwrap()), (640, 360));
}

#[tokio::test]
async fn an_oembed_thumbnail_outranks_the_open_graph_card() {
    let pipeline = Pipeline {
        transport: TestTransport::with(vec![
            (
                "https://video.example.invalid/watch/1",
                page(
                    200,
                    concat!(
                        r#"<title>A video</title>"#,
                        r#"<link rel="alternate" type="application/json+oembed" href="https://video.example.invalid/oembed?id=1">"#,
                        r#"<meta property="og:image" content="/template-card.png">"#,
                    ),
                ),
            ),
            (
                "https://video.example.invalid/oembed?id=1",
                WireResponse {
                    status: 200,
                    content_type: Some("application/json".to_owned()),
                    location: None,
                    body: br#"{"title":"A video","thumbnail_url":"https://img.example.invalid/frame.jpg","thumbnail_width":1280,"thumbnail_height":720}"#.to_vec(),
                },
            ),
            (
                "https://img.example.invalid/frame.jpg",
                image(200, "image/jpeg", jpeg_bytes(1280, 720)),
            ),
        ]),
    };
    let preview = pipeline
        .fetch("https://video.example.invalid/watch/1")
        .await
        .unwrap();

    assert!(preview.image.is_some());
    // The oEmbed picture won; the template card was never even requested.
    let went_to_card = pipeline
        .transport
        .seen
        .lock()
        .unwrap()
        .iter()
        .any(|(url, _)| url.ends_with("/template-card.png"));
    assert!(!went_to_card);
}

#[tokio::test]
async fn a_failing_oembed_endpoint_falls_through_to_open_graph() {
    let transport = TestTransport::with(vec![
        (
            "https://video.example.invalid/watch/2",
            page(
                200,
                concat!(
                    r#"<link rel="alternate" type="application/json+oembed" href="/broken-oembed">"#,
                    r#"<meta property="og:image" content="/card.png">"#,
                ),
            ),
        ),
        (
            "https://video.example.invalid/broken-oembed",
            image(500, "text/html", Vec::new()),
        ),
        (
            "https://video.example.invalid/card.png",
            image(200, "image/png", png_bytes(600, 400)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://video.example.invalid/watch/2")
        .await
        .unwrap();

    assert!(preview.image.is_some());
}

#[tokio::test]
async fn twitter_serves_where_open_graph_never_spoke() {
    let document = concat!(
        "<title>Old site</title>",
        r#"<meta name="twitter:image" content="/twitter-card.png">"#,
    );
    let transport = TestTransport::with(vec![
        ("https://old.example.invalid/", page(200, document)),
        (
            "https://old.example.invalid/twitter-card.png",
            image(200, "image/png", png_bytes(440, 220)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://old.example.invalid/")
        .await
        .unwrap();

    assert_eq!(preview.title.as_deref(), Some("Old site"));
    assert!(preview.image.is_some());
}

#[tokio::test]
async fn open_graph_is_asked_before_twitter() {
    let document = concat!(
        r#"<meta name="twitter:image" content="/twitter.png">"#,
        r#"<meta property="og:image" content="/og.png">"#,
    );
    let pipeline = Pipeline {
        transport: TestTransport::with(vec![
            ("https://example.invalid/", page(200, document)),
            (
                "https://example.invalid/og.png",
                image(200, "image/png", png_bytes(500, 500)),
            ),
            (
                "https://example.invalid/twitter.png",
                image(200, "image/png", png_bytes(500, 500)),
            ),
        ]),
    };
    let _ = pipeline.fetch("https://example.invalid/").await.unwrap();

    let went_to = |path: &str| {
        pipeline
            .transport
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|(url, _)| url == path)
    };
    assert!(went_to("https://example.invalid/og.png"));
    // Twitter's candidate was held ready but never needed.
    assert!(!went_to("https://example.invalid/twitter.png"));
}

#[tokio::test]
async fn structured_data_is_the_last_word_when_tags_are_silent() {
    let document = concat!(
        "<title>Research</title>",
        r#"<script type="application/ld+json">{"@type":"Article","image":"https://cdn.example.invalid/paper.jpg"}</script>"#,
    );
    let transport = TestTransport::with(vec![
        ("https://journal.example.invalid/a", page(200, document)),
        (
            "https://cdn.example.invalid/paper.jpg",
            image(200, "image/jpeg", jpeg_bytes(900, 500)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://journal.example.invalid/a")
        .await
        .unwrap();

    assert!(preview.image.is_some());
}

#[tokio::test]
async fn a_disqualified_first_candidate_gives_way_to_the_second() {
    // The first nomination declares itself banner-shaped; fetching it would be
    // wasted motion, so the walk starts at the second.
    let document = concat!(
        r#"<meta property="og:image" content="/banner.png">"#,
        r#"<meta property="og:image:width" content="1200">"#,
        r#"<meta property="og:image:height" content="90">"#,
        r#"<meta property="og:image" content="/real.png">"#,
    );
    let pipeline = Pipeline {
        transport: TestTransport::with(vec![
            ("https://example.invalid/", page(200, document)),
            (
                "https://example.invalid/banner.png",
                image(200, "image/png", png_bytes(1200, 90)),
            ),
            (
                "https://example.invalid/real.png",
                image(200, "image/png", png_bytes(600, 400)),
            ),
        ]),
    };
    let preview = pipeline.fetch("https://example.invalid/").await.unwrap();

    let went_to_banner = pipeline
        .transport
        .seen
        .lock()
        .unwrap()
        .iter()
        .any(|(url, _)| url.ends_with("/banner.png"));
    assert!(!went_to_banner);
    assert_eq!(dimension_of(preview.image.as_deref().unwrap()), (600, 400));
}

#[tokio::test]
async fn a_tracking_pixel_nominated_as_a_card_is_refused_after_being_seen() {
    // Declarations said nothing, so the pixel had to be fetched to be judged.
    let document = concat!(
        r#"<meta property="og:image" content="/pixel.gif">"#,
        r#"<meta property="og:image" content="/real.png">"#,
    );
    let pipeline = Pipeline {
        transport: TestTransport::with(vec![
            ("https://example.invalid/", page(200, document)),
            (
                "https://example.invalid/pixel.gif",
                image(200, "image/gif", png_bytes(1, 1)),
            ),
            (
                "https://example.invalid/real.png",
                image(200, "image/png", png_bytes(600, 400)),
            ),
        ]),
    };
    let preview = pipeline.fetch("https://example.invalid/").await.unwrap();

    assert_eq!(dimension_of(preview.image.as_deref().unwrap()), (600, 400));
}

#[tokio::test]
async fn a_picture_smaller_than_any_card_is_rejected_on_its_own_bytes() {
    let document = og_page("/logo.png");
    let transport = TestTransport::with(vec![
        ("https://example.invalid/", page(200, &document)),
        (
            "https://example.invalid/logo.png",
            image(200, "image/png", png_bytes(64, 64)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://example.invalid/")
        .await
        .unwrap();

    assert!(preview.image.is_none());
    // The title survived the refusal of the picture.
    assert_eq!(preview.title.as_deref(), Some("Synthetic Page"));
}

#[tokio::test]
async fn a_banner_shaped_card_is_rejected_by_its_proportions() {
    let document = og_page("/rule.png");
    let transport = TestTransport::with(vec![
        ("https://example.invalid/", page(200, &document)),
        (
            "https://example.invalid/rule.png",
            image(200, "image/png", png_bytes(800, 60)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://example.invalid/")
        .await
        .unwrap();

    assert!(preview.image.is_none());
}

#[tokio::test]
async fn a_link_that_is_itself_a_picture_is_kept_whatever_its_size() {
    // There the image is the content; the card rules do not apply.
    let transport = TestTransport::with(vec![(
        "https://example.invalid/photo.jpg",
        image(200, "image/jpeg", jpeg_bytes(96, 96)),
    )]);
    let preview = Pipeline { transport }
        .fetch("https://example.invalid/photo.jpg")
        .await
        .unwrap();

    assert_eq!(preview.image_mime.as_deref(), Some("image/jpeg"));
    assert_eq!(dimension_of(preview.image.as_deref().unwrap()), (96, 96));
}

#[tokio::test]
async fn a_picture_without_a_content_type_is_recognised_by_its_first_bytes() {
    let transport = TestTransport::with(vec![(
        "https://example.invalid/raw",
        WireResponse {
            status: 200,
            content_type: Some("application/octet-stream".to_owned()),
            location: None,
            body: jpeg_bytes(300, 200),
        },
    )]);
    let preview = Pipeline { transport }
        .fetch("https://example.invalid/raw")
        .await
        .unwrap();

    assert_eq!(preview.image_mime.as_deref(), Some("image/jpeg"));
}

#[tokio::test]
async fn a_missing_file_served_as_a_page_and_status_200_is_not_a_picture() {
    // The classic CDN lie: a 404 rendered as HTML, served with status 200.
    let transport = TestTransport::with(vec![(
        "https://cdn.example.invalid/gone.jpg",
        WireResponse {
            status: 200,
            content_type: Some("text/html".to_owned()),
            location: None,
            body: b"<html><body>not found</body></html>".to_vec(),
        },
    )]);
    let preview = Pipeline { transport }
        .fetch("https://cdn.example.invalid/gone.jpg")
        .await
        .unwrap();

    assert!(preview.image.is_none());
}

#[tokio::test]
async fn relative_addresses_resolve_against_where_the_redirect_landed() {
    let transport = TestTransport::with(vec![
        (
            "https://origin.example.invalid/a",
            redirect(301, "https://final.example.invalid/deep/b"),
        ),
        (
            "https://final.example.invalid/deep/b",
            page(
                200,
                concat!(
                    "<title>Landed</title>",
                    r#"<link rel="icon" href="mark.ico">"#,
                    r#"<meta property="og:image" content="../card.png">"#,
                ),
            ),
        ),
        (
            "https://final.example.invalid/deep/mark.ico",
            image(200, "image/png", png_bytes(16, 16)),
        ),
        (
            "https://final.example.invalid/card.png",
            image(200, "image/png", png_bytes(600, 400)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://origin.example.invalid/a")
        .await
        .unwrap();

    assert_eq!(preview.title.as_deref(), Some("Landed"));
    assert!(preview.image.is_some());
}

#[tokio::test]
async fn a_base_declaration_moves_where_relative_addresses_resolve_from() {
    let document = concat!(
        r#"<base href="https://assets.example.invalid/v9/">"#,
        r#"<meta property="og:image" content="card.png">"#,
    );
    let transport = TestTransport::with(vec![
        ("https://page.example.invalid/x", page(200, document)),
        (
            "https://assets.example.invalid/v9/card.png",
            image(200, "image/png", png_bytes(600, 400)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://page.example.invalid/x")
        .await
        .unwrap();

    assert!(preview.image.is_some());
}

#[tokio::test]
async fn a_redirect_into_this_network_is_refused_at_that_hop() {
    let transport = TestTransport::with(vec![(
        "https://outside.example.invalid/",
        redirect(302, "http://192.168.1.1/admin"),
    )]);
    let error = Pipeline { transport }
        .fetch("https://outside.example.invalid/")
        .await
        .unwrap_err();

    assert_eq!(error, LinkPreviewError::NotFetchable);
}

#[tokio::test]
async fn a_redirect_chain_longer_than_the_limit_stops() {
    let transport = TestTransport::with(
        (0..12)
            .map(|index| {
                (
                    Box::leak(format!("https://hop{index}.example.invalid/").into_boxed_str())
                        as &str,
                    redirect(302, &format!("https://hop{}.example.invalid/", index + 1)),
                )
            })
            .collect::<Vec<_>>(),
    );
    let error = Pipeline { transport }
        .fetch("https://hop0.example.invalid/")
        .await
        .unwrap_err();

    assert_eq!(error, LinkPreviewError::TooManyRedirects);
}

#[tokio::test]
async fn tracking_parameters_do_not_reach_the_wire() {
    // The route answers only the cleaned address: had the trackers survived,
    // the request would have carried them and found no answer.
    let transport = TestTransport::with(vec![(
        "https://example.invalid/article?id=5",
        page(200, "<title>Article</title>"),
    )]);
    let preview = Pipeline { transport }
        .fetch("https://example.invalid/article?utm_source=news&fbclid=abc&id=5")
        .await
        .unwrap();

    assert_eq!(preview.title.as_deref(), Some("Article"));
}

#[tokio::test]
async fn a_refusal_by_status_is_asked_again_under_a_crawlers_name() {
    let transport = TestTransport::with(vec![(
        "https://gated.example.invalid/",
        page(403, "<title>Consent wall</title>"),
    )]);
    let pipeline = Pipeline { transport };
    let _ = pipeline.fetch("https://gated.example.invalid/").await;

    let agents = pipeline
        .transport
        .requests_for("https://gated.example.invalid/");
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0], OWN_USER_AGENT);
    assert_eq!(agents[1], CRAWLER_USER_AGENT);
}

#[tokio::test]
async fn an_honest_answer_is_never_asked_twice() {
    let transport = TestTransport::with(vec![(
        "https://calm.example.invalid/",
        page(200, "<title>Calm</title>"),
    )]);
    let pipeline = Pipeline { transport };
    pipeline
        .fetch("https://calm.example.invalid/")
        .await
        .unwrap();

    assert_eq!(
        pipeline
            .transport
            .requests_for("https://calm.example.invalid/")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_touch_icon_is_preferred_over_the_favicon() {
    let document = concat!(
        r#"<link rel="apple-touch-icon" href="/touch.png">"#,
        r#"<link rel="icon" href="/favicon.png">"#,
    );
    let pipeline = Pipeline {
        transport: TestTransport::with(vec![
            ("https://example.invalid/", page(200, document)),
            (
                "https://example.invalid/touch.png",
                image(200, "image/png", png_bytes(180, 180)),
            ),
            (
                "https://example.invalid/favicon.png",
                image(200, "image/png", png_bytes(16, 16)),
            ),
        ]),
    };
    let preview = pipeline.fetch("https://example.invalid/").await.unwrap();

    assert!(preview.icon.is_some());
    let went_to_favicon = pipeline
        .transport
        .seen
        .lock()
        .unwrap()
        .iter()
        .any(|(url, _)| url.ends_with("/favicon.png"));
    assert!(!went_to_favicon);
}

#[tokio::test]
async fn the_conventional_path_is_tried_when_nothing_is_declared() {
    let transport = TestTransport::with(vec![
        (
            "https://bare.example.invalid/",
            page(200, "<title>Bare</title>"),
        ),
        (
            "https://bare.example.invalid/favicon.ico",
            image(200, "image/png", png_bytes(16, 16)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://bare.example.invalid/")
        .await
        .unwrap();

    assert!(preview.icon.is_some());
}

#[tokio::test]
async fn a_declared_icon_that_fails_falls_through_to_the_conventional_path() {
    let document = r#"<link rel="icon" href="/missing.png">"#;
    let transport = TestTransport::with(vec![
        ("https://example.invalid/", page(200, document)),
        (
            "https://example.invalid/favicon.ico",
            image(200, "image/png", png_bytes(16, 16)),
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://example.invalid/")
        .await
        .unwrap();

    assert!(preview.icon.is_some());
}

#[tokio::test]
async fn a_page_in_windows_1250_titles_correctly() {
    let mut body = b"<html><head><meta charset=\"windows-1250\"><title>".to_vec();
    body.extend_from_slice(&[0xB3]); // ł
    body.extend_from_slice(b" od zera</title></html>");
    let transport = TestTransport::with(vec![(
        "https://stary.example.invalid/",
        WireResponse {
            status: 200,
            content_type: Some("text/html".to_owned()),
            location: None,
            body,
        },
    )]);
    let preview = Pipeline { transport }
        .fetch("https://stary.example.invalid/")
        .await
        .unwrap();

    assert_eq!(preview.title.as_deref(), Some("ł od zera"));
}

#[tokio::test]
async fn an_oembed_title_fills_in_where_the_page_has_none() {
    let document = r#"<link rel="alternate" type="application/json+oembed" href="/oembed">"#;
    let transport = TestTransport::with(vec![
        ("https://song.example.invalid/track", page(200, document)),
        (
            "https://song.example.invalid/oembed",
            WireResponse {
                status: 200,
                content_type: Some("application/json".to_owned()),
                location: None,
                body: br#"{"title":"One song","thumbnail_url":"https://song.example.invalid/art.jpg"}"#
                    .to_vec(),
            },
        ),
        ("https://song.example.invalid/art.jpg", image(200, "image/jpeg", jpeg_bytes(640, 640))),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://song.example.invalid/track")
        .await
        .unwrap();

    assert_eq!(preview.title.as_deref(), Some("One song"));
    assert!(preview.image.is_some());
}

#[tokio::test]
async fn the_pages_own_title_outranks_an_oembeds() {
    let document = concat!(
        "<title>The real title</title>",
        r#"<link rel="alternate" type="application/json+oembed" href="/oembed">"#,
    );
    let transport = TestTransport::with(vec![
        ("https://mixed.example.invalid/", page(200, document)),
        (
            "https://mixed.example.invalid/oembed",
            WireResponse {
                status: 200,
                content_type: Some("application/json".to_owned()),
                location: None,
                body: br#"{"title":"Endpoint's title"}"#.to_vec(),
            },
        ),
    ]);
    let preview = Pipeline { transport }
        .fetch("https://mixed.example.invalid/")
        .await
        .unwrap();

    assert_eq!(preview.title.as_deref(), Some("The real title"));
}

#[tokio::test]
async fn an_empty_answer_is_still_an_answer_rather_than_an_error() {
    let transport = TestTransport::with(vec![(
        "https://quiet.example.invalid/",
        page(200, "<html><body>nothing here</body></html>"),
    )]);
    let preview = Pipeline { transport }
        .fetch("https://quiet.example.invalid/")
        .await
        .unwrap();

    assert!(preview.is_empty());
}

#[tokio::test]
async fn a_server_error_is_a_refusal() {
    let transport = TestTransport::with(vec![(
        "https://sick.example.invalid/",
        page(500, "server on fire"),
    )]);
    let error = Pipeline { transport }
        .fetch("https://sick.example.invalid/")
        .await
        .unwrap_err();

    assert_eq!(error, LinkPreviewError::Refused);
}

#[tokio::test]
async fn an_address_inside_this_network_never_reaches_the_transport() {
    let transport = TestTransport::with(vec![]);
    let error = Pipeline { transport }
        .fetch("http://169.254.169.254/latest/meta-data/")
        .await
        .unwrap_err();

    assert_eq!(error, LinkPreviewError::NotFetchable);
}

#[tokio::test]
async fn something_that_is_not_an_address_is_not_fetchable() {
    let transport = TestTransport::with(vec![]);
    let error = Pipeline { transport }
        .fetch("definitely not a url")
        .await
        .unwrap_err();

    assert_eq!(error, LinkPreviewError::NotFetchable);
}

// ---------------------------------------------------------------------------
// Pure helpers whose rules deserve their own cases.
// ---------------------------------------------------------------------------

#[test]
fn nominated_dimension_rules_admit_cards_and_refuse_marks_and_banners() {
    // Ordinary cards.
    assert!(nominated_dimensions_pass(1200, 630));
    assert!(nominated_dimensions_pass(600, 600));
    assert!(nominated_dimensions_pass(200, 200)); // exactly at the floor
    // Tracking pixels and logos.
    assert!(!nominated_dimensions_pass(1, 1));
    assert!(!nominated_dimensions_pass(199, 199));
    assert!(!nominated_dimensions_pass(64, 64));
    // Banners and rules.
    assert!(!nominated_dimensions_pass(1200, 90));
    assert!(!nominated_dimensions_pass(90, 1200));
    // Ratio edges hold exactly.
    assert!(nominated_dimensions_pass(1000, 250)); // 4:1, allowed
    assert!(!nominated_dimensions_pass(1000, 249)); // past 4:1
    assert!(nominated_dimensions_pass(300, 1000)); // 0.3, allowed
    assert!(!nominated_dimensions_pass(299, 1000)); // below three tenths
}

#[test]
fn magic_bytes_name_the_common_formats() {
    let mut webp = b"RIFF\x00\x00\x00\x00WEBPVP8 ".to_vec();
    assert_eq!(sniffed_image_mime(&webp), Some("image/webp"));
    assert_eq!(
        sniffed_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]),
        Some("image/jpeg")
    );
    assert_eq!(
        sniffed_image_mime(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A]),
        Some("image/png")
    );
    assert_eq!(sniffed_image_mime(b"GIF89a...."), Some("image/gif"));
    assert_eq!(
        sniffed_image_mime(b"<svg xmlns=\"\"></svg>"),
        Some("image/svg+xml")
    );
    assert_eq!(sniffed_image_mime(b"<html>no</html>"), None);
    webp.truncate(8); // too short to carry the format mark
    assert_eq!(sniffed_image_mime(&webp), None);
}

#[test]
fn svg_is_recognised_by_declaration_or_body_but_not_confused_with_markup() {
    assert!(is_svg("image/svg+xml", b""));
    assert!(is_svg("image/png", b"<?xml version=\"1.0\"?><svg/>"));
    assert!(!is_svg("image/png", b"<html><svg-in-text></html>"));
}

#[test]
fn text_caps_cut_on_characters_and_drop_only_whitespace() {
    assert_eq!(cap_text("  hello  ", 3).as_deref(), Some("hel"));
    // Multibyte letters survive whole.
    assert_eq!(cap_text("żółć", 3).as_deref(), Some("żół"));
    assert_eq!(cap_text("   ", 10), None);
    assert_eq!(cap_text("", 10), None);
}

#[test]
fn the_head_close_tag_is_found_however_it_is_written() {
    let haystack = b"<html><head><title>x</title></head><body>".to_vec();
    let found = haystack
        .windows(HEAD_CLOSE_TAG.len())
        .any(|window| window.eq_ignore_ascii_case(HEAD_CLOSE_TAG.as_bytes()));
    assert!(found);
    assert!(
        !b"<html><head><title>never closed"
            .windows(HEAD_CLOSE_TAG.len())
            .any(|window| window.eq_ignore_ascii_case(HEAD_CLOSE_TAG.as_bytes()))
    );
}

#[test]
fn a_local_description_needs_no_network() {
    assert_eq!(
        describe_locally("https://www.linkedin.com/in/someone/"),
        Some(("linkedin.com".to_owned(), "/in/someone/".to_owned()))
    );
    assert_eq!(
        describe_locally("https://example.com/search?q=1"),
        Some(("example.com".to_owned(), "/search?q=1".to_owned()))
    );
}

#[test]
fn something_that_is_not_a_web_address_describes_as_nothing() {
    for raw in [
        "mailto:someone@example.com",
        "not a url",
        "file:///etc/hosts",
    ] {
        assert_eq!(describe_locally(raw), None, "{raw}");
    }
}

#[test]
fn an_address_inside_this_network_is_refused_before_a_socket_is_opened() {
    // Resolution is attempted, the answer is judged, and nothing connects.
    assert_eq!(
        resolve_public_address("localhost", 80),
        Err(LinkPreviewError::NotFetchable)
    );
}

#[test]
fn every_error_names_a_code_and_never_an_address() {
    for error in [
        LinkPreviewError::NotFetchable,
        LinkPreviewError::Unreachable,
        LinkPreviewError::Refused,
        LinkPreviewError::TooManyRedirects,
        LinkPreviewError::ClientUnavailable,
    ] {
        let rendered = error.to_string();
        assert!(rendered.starts_with("link_"), "{rendered}");
        assert!(!rendered.contains("://"), "{rendered}");
    }
}

#[test]
fn a_preview_holding_only_a_picture_still_counts_as_something() {
    let preview = LinkPreview {
        image: Some(vec![1, 2, 3]),
        image_mime: Some("image/jpeg".to_owned()),
        ..LinkPreview::default()
    };

    assert!(!preview.is_empty());
}

#[test]
fn an_empty_preview_is_recognised_as_nothing_worth_keeping() {
    assert!(LinkPreview::default().is_empty());
    assert!(
        !LinkPreview {
            title: Some("Synthetic".to_owned()),
            ..LinkPreview::default()
        }
        .is_empty()
    );
}

#[tokio::test]
async fn a_manifest_supplies_an_icon_when_the_markup_named_none() {
    let document = r#"<link rel="manifest" href="/app.webmanifest">"#;
    let pipeline = Pipeline {
        transport: TestTransport::with(vec![
            ("https://example.invalid/", page(200, document)),
            (
                "https://example.invalid/app.webmanifest",
                WireResponse {
                    status: 200,
                    content_type: Some("application/manifest+json".to_owned()),
                    location: None,
                    body: br#"{"icons":[
                        {"src":"/i/48.png","sizes":"48x48"},
                        {"src":"/i/192.png","sizes":"192x192"}
                    ]}"#
                    .to_vec(),
                },
            ),
            (
                "https://example.invalid/i/192.png",
                image(200, "image/png", png_bytes(192, 192)),
            ),
        ]),
    };

    let preview = pipeline.fetch("https://example.invalid/").await.unwrap();

    assert!(preview.icon.is_some());
    // The largest, not the first the manifest happened to list.
    assert_eq!(
        pipeline
            .transport
            .requests_for("https://example.invalid/i/48.png"),
        Vec::<&str>::new()
    );
}

#[tokio::test]
async fn a_manifest_is_not_fetched_when_the_page_named_an_icon() {
    // The cost test rather than the result test. An icon is not worth a second
    // round trip to every site the user has ever copied, so the request must
    // not happen at all for the pages that already answered.
    let document = concat!(
        r#"<link rel="apple-touch-icon" href="/touch.png">"#,
        r#"<link rel="manifest" href="/app.webmanifest">"#,
    );
    let pipeline = Pipeline {
        transport: TestTransport::with(vec![
            ("https://example.invalid/", page(200, document)),
            (
                "https://example.invalid/touch.png",
                image(200, "image/png", png_bytes(180, 180)),
            ),
        ]),
    };

    let preview = pipeline.fetch("https://example.invalid/").await.unwrap();

    assert!(preview.icon.is_some());
    assert_eq!(
        pipeline
            .transport
            .requests_for("https://example.invalid/app.webmanifest"),
        Vec::<&str>::new(),
        "a page that named its own icon must cost no extra request"
    );
}

#[tokio::test]
async fn a_manifest_icon_pointing_somewhere_private_is_refused() {
    // The manifest is a stranger's document naming addresses, exactly like the
    // page itself.
    //
    // What actually enforces this is the single gate at the top of `exchange`,
    // which every request in the pipeline passes through — so this test cannot
    // be made to fail by weakening the manifest path alone, and it is not
    // evidence that the checks there are load-bearing. It is here to hold the
    // manifest path to the property end to end, and it would catch a future
    // fetch that reached the wire without going through that gate.
    let document = r#"<link rel="manifest" href="/app.webmanifest">"#;
    let pipeline = Pipeline {
        transport: TestTransport::with(vec![
            ("https://example.invalid/", page(200, document)),
            (
                "https://example.invalid/app.webmanifest",
                WireResponse {
                    status: 200,
                    content_type: Some("application/manifest+json".to_owned()),
                    location: None,
                    body: br#"{"icons":[{"src":"http://169.254.169.254/latest/meta-data/","sizes":"512x512"}]}"#.to_vec(),
                },
            ),
        ]),
    };

    let preview = pipeline.fetch("https://example.invalid/").await.unwrap();

    assert!(preview.icon.is_none());
    assert_eq!(
        pipeline
            .transport
            .requests_for("http://169.254.169.254/latest/meta-data/"),
        Vec::<&str>::new(),
        "the address policy must hold for what a manifest names"
    );
}

#[tokio::test]
async fn a_manifest_outranks_the_small_marks_below_it() {
    // The ordering test, and the reason it exists: `/favicon.ico` answers on
    // nearly every site, so a manifest consulted after it would never be
    // consulted at all.
    let document = concat!(
        r#"<link rel="icon" href="/tiny.png">"#,
        r#"<link rel="manifest" href="/app.webmanifest">"#,
    );
    let pipeline = Pipeline {
        transport: TestTransport::with(vec![
            ("https://example.invalid/", page(200, document)),
            (
                "https://example.invalid/app.webmanifest",
                WireResponse {
                    status: 200,
                    content_type: Some("application/manifest+json".to_owned()),
                    location: None,
                    body: br#"{"icons":[{"src":"/i/512.png","sizes":"512x512"}]}"#.to_vec(),
                },
            ),
            (
                "https://example.invalid/i/512.png",
                image(200, "image/png", png_bytes(512, 512)),
            ),
            (
                "https://example.invalid/tiny.png",
                image(200, "image/png", png_bytes(16, 16)),
            ),
            (
                "https://example.invalid/favicon.ico",
                image(200, "image/png", png_bytes(16, 16)),
            ),
        ]),
    };

    let preview = pipeline.fetch("https://example.invalid/").await.unwrap();

    assert!(preview.icon.is_some());
    for small in [
        "https://example.invalid/tiny.png",
        "https://example.invalid/favicon.ico",
    ] {
        assert_eq!(
            pipeline.transport.requests_for(small),
            Vec::<&str>::new(),
            "{small} was asked for although the manifest had already answered"
        );
    }
}
