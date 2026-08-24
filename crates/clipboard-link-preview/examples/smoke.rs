//! Live check against real sites. Run by hand: `cargo run -p clipboard-link-preview --example smoke -- <url>...`
#![forbid(unsafe_code)]

#[tokio::main]
async fn main() {
    let urls: Vec<String> = std::env::args().skip(1).collect();
    let urls = if urls.is_empty() {
        vec![
            "https://github.com/rust-lang/rust".to_owned(),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_owned(),
            "https://example.com/?utm_source=test&fbclid=xyz".to_owned(),
        ]
    } else {
        urls
    };
    let fetcher = clipboard_link_preview::LinkPreviewFetcher::new().unwrap();
    for url in urls {
        match fetcher.fetch(&url).await {
            Ok(preview) => {
                let image = preview.image.as_ref().map(|bytes| bytes.len());
                println!(
                    "{url}\n  title: {:?}\n  icon: {:?} bytes ({:?})\n  image: {:?} bytes ({:?})",
                    preview.title,
                    preview.icon.as_ref().map(|b| b.len()),
                    preview.icon_mime,
                    image,
                    preview.image_mime
                );
            }
            Err(error) => println!("{url}\n  error: {error}"),
        }
    }
}
